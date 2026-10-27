# codewhale-web

Documentation and community site for [Codewhale](https://github.com/codewhale-hq/CodeWhale) — lives at **codewhale.net**.

Next.js 16 (App Router) + Tailwind, deployed to Cloudflare Workers via [`@opennextjs/cloudflare`](https://opennext.js.org/cloudflare). Curated "Today's Dispatch" content is regenerated every 6 hours by a Cloudflare Cron Trigger that calls `deepseek-flash` to summarise recent repo activity, and stored in Workers KV.

## Local dev

```bash
cd web
npm install
cp .env.example .env.local   # fill in the keys you have
npm run dev                  # http://localhost:3000
```

Env (mirrors `.env.example`):

| Variable                    | What                                                             | Required?            |
| --------------------------- | ---------------------------------------------------------------- | -------------------- |
| `DEEPSEEK_API_KEY`          | DeepSeek platform key (`sk-...`)                                 | only for the `/api/cron` tasks (summarization + community agent) |
| `GITHUB_TOKEN`              | Fine-grained PAT, public-repo read scope                         | optional (raises rate limit 60 → 5000 req/h) |
| `GITHUB_REPO`               | Defaults to `codewhale-hq/CodeWhale`                                   | optional             |
| `CRON_SECRET`               | Shared secret for manual `/api/cron` invocation                  | optional (Cloudflare cron triggers don't need it) |
| `DEEPSEEK_MODEL`            | Defaults to `deepseek-flash`                                     | optional             |
| `DEEPSEEK_BASE_URL`         | Defaults to `https://api.deepseek.com`                           | optional             |
| `MAINTAINER_TOKEN`          | Admin panel auth; enter it in the `/admin` login form            | only for `/admin`    |
| `MAINTAINER_GITHUB_PAT`     | PAT with `issues:write`, for posting comments via `/admin`       | only for `/admin` posting |
| `NEXT_PUBLIC_GITEE_ENABLED` | Set to `1` once the Gitee mirror exists; blank hides Gitee links | optional             |

The site renders fine without any of them — `Today's Dispatch` falls back to a static editorial; the GitHub feed shows "feed not yet loaded".

## Deploy to Cloudflare

The `.github/workflows/web.yml` workflow runs only for a maintainer-triggered
`workflow_dispatch`. Ordinary pushes and pull requests do not start it.
A dispatch on `main` runs the web checks and production build before deploying.
At the approved final publication step, record the exact 40-character
`origin/main` SHA and trigger that ref:

```bash
git fetch origin main
git rev-parse origin/main
gh workflow run web.yml --repo codewhale-hq/CodeWhale --ref main
```

Confirm that the dispatched run resolved to the recorded SHA before treating
its result as evidence for that candidate. The deploy job checks out the exact
dispatch SHA even if `main` advances while the run is queued.

The manual job records the pre-deploy source drift, builds the OpenNext bundle,
deploys only after the protected Cloudflare inputs pass, and then requires the
public `/api/facts` receipt to report the exact workflow SHA. A credential-free
local comparison is available without starting a deployment:

```bash
npm run compare:deployed-facts -- --expected-revision <exact-40-character-sha>
```

The Worker configuration now lives in `cloudflare.config.ts`; it retains the
existing namespace IDs, domains, cron schedules and SQLite Durable Object.
`npm ci` installs pinned `cf` and Cloudflare bundler versions. Use Node 22.18 or
newer. OpenNext currently consumes a generated legacy configuration in the
ignored `.cloudflare/` directory, derived from that same source with
Cloudflare's configuration SDK.

```bash
npm run build:cloudflare          # one OpenNext build, then native cf Build Output
npx cf deploy --prebuilt --dry-run # local validation; no upload
npm run preview                   # local Worker with populated local cache
```

The pinned cf beta's Next.js detection currently invokes plain `next build`,
which does not produce the OpenNext Worker. `build:cloudflare` therefore runs
OpenNext first and invokes cf's installed bundler (`cf-wrangler.js`) to emit
Build Output. This is a build step; production deployment uses `cf deploy
--prebuilt`. Do not add a custom post-cache build to `wrangler.config.ts`:
rebuilding there changes the cache identity after OpenNext populates it.

After production approval, the manual workflow runs `npm run deploy`,
populates the remote OpenNext cache, and passes the exact prebuilt bundle to
cf. Existing secrets remain managed in Cloudflare; namespace provisioning,
secret changes and domain changes require separate approval. The predeploy
guard rejects unset namespace IDs to avoid accidentally replacing live data.
Use `npx cf cli search` to find the current resource-management commands.

## What's where

Pages are bilingual by default: each `app/[locale]/` page renders both
English and Chinese from the same file, keyed by the `[locale]` segment
(see `lib/i18n/config.ts`). Copy changes must update both locales. The
v0.9.2 wave adds routed **partial** locales (ja, vi, ko, ru, uk, es, pt-BR):
their shared chrome (nav/footer/switcher) and home-page copy live in
`lib/i18n/dictionaries/<code>/` (checked by `npm run check:locales`), and
everything else falls back to the English copy. Routing, middleware
detection (`lib/i18n/detect.ts`), sitemap, and hreflang all derive from the
one registry.

```
web/
├── app/
│   ├── globals.css             ocean portal, docs layout, type, and shared surfaces
│   ├── [locale]/               10 routed locales; zh has native page bodies,
│   │                           the rest fall back to the English body
│   │   ├── layout.tsx          root + locale layout: html shell, fonts, nav, footer
│   │   ├── page.tsx            home — hero, ticker, proof, decides, workflow,
│   │   │                       start, boundaries, surfaces, install band, community
│   │   ├── install/page.tsx    per-OS install with auto-detection
│   │   ├── docs/page.tsx       modes / tools / approval / config / mcp / providers
│   │   ├── faq/page.tsx        frequently asked questions
│   │   ├── feed/page.tsx       live mirror of issues + PRs
│   │   ├── roadmap/page.tsx    shipped / underway / considered / ruled out
│   │   ├── contribute/page.tsx how to PR + house rules + dev loop
│   │   └── admin/              maintainer panel (page.tsx + admin-client.tsx)
│   └── api/
│       ├── cron/route.ts          cron tasks: curate, triage, facts-drift, …
│       ├── facts/route.ts         public source/deployment receipt
│       ├── github/feed/route.ts   cached JSON endpoint
│       └── admin/                 login, logout, post (MAINTAINER_TOKEN-gated)
├── data/
│   └── latest-published-release.json  manually advanced only after publication
├── components/
│   ├── native-terminal-gallery.tsx  six captured native terminal views
│   ├── nav.tsx                 sticky header w/ date strip + CJK accents
│   ├── footer.tsx              dense 5-column footer
│   ├── whale.tsx               shared Codewhale mark
│   ├── ticker.tsx              live wire: merges, issues, releases + handles
│   ├── feed-card.tsx           one issue/PR card
│   ├── locale-switcher.tsx     N-locale dropdown; partial packs grouped last
│   └── install-*.tsx           install page blocks (binary, code block, tiles)
├── lib/
│   ├── types.ts                shared types
│   ├── i18n/                   locale config, en/zh dictionaries
│   ├── github.ts               REST client + relative-time formatter
│   ├── deepseek.ts             v4-flash chat client + curate() prompt
│   ├── facts.ts                getFacts(): KV value, else build-time FACTS
│   ├── facts.generated.ts      GENERATED — do not edit by hand
│   ├── changelog.generated.ts  GENERATED at build/test time, untracked
│   ├── install-guide.generated.ts GENERATED at build/test time, untracked
│   ├── facts-drift.ts          runtime re-derivation for the drift cron
│   ├── community-agent.ts      triage / pr-review / digest cron tasks
│   └── kv.ts                   Cloudflare KV access via OpenNext bindings
├── scripts/
│   ├── derive-facts.mjs        prebuild: repo sources → lib/facts.generated.ts
│   ├── derive-changelog.mjs    prebuild + vitest setup: CHANGELOG.md → lib/changelog.generated.ts
│   ├── derive-install.mjs      prebuild + vitest setup: docs/INSTALL.md → lib/install-guide.generated.ts
│   ├── compare-deployed-facts.mjs credential-free exact-SHA receipt check
│   └── check-kv-id.mjs         predeploy guard for KV namespace ids
├── cloudflare.config.ts        Worker config + existing bindings, exports and cron
├── wrangler.config.ts          cf bundler options (no separate Worker config)
├── open-next.config.ts         OpenNext adapter config
└── tailwind.config.ts          design tokens
```

## Facts pipeline

Mechanical facts (version, provider list, sandbox backends, crate names,
default model, Node engines) are never hand-written into pages:

1. **Build time** — `scripts/derive-facts.mjs` runs as `prebuild` (and before
   `npm run dev`), parses the parent repo (`Cargo.toml`, `crates/tui/src/config.rs`,
   `crates/sandbox/src/lib.rs`, `npm/codewhale/package.json`) and writes
   `lib/facts.generated.ts`. Never edit that file by hand.
2. **Published release** — `data/latest-published-release.json` records the
   latest GitHub Release separately from the source candidate. Install commands
   use this published tag; they never turn the workspace version into a release
   before publication. The credential-free deployed-facts comparison checks the
   record against the public receipt.
3. **Runtime** — the `/api/cron?task=facts-drift` cron (`lib/facts-drift.ts`)
   resolves an exact `main` revision, derives every source fact from that SHA,
   and writes changes to `CURATED_KV` under `facts:current`. Pages accept that
   snapshot only when its source provenance is the same as or newer than the
   deployed build. Legacy, malformed, or older KV data cannot replace newer
   build facts; published-release metadata is resolved independently. Public
   fact pages revalidate their cached HTML every five minutes.

`/api/facts` exposes only public provenance and counts: deployed/resolved source
revision, version, provider count, tool count, selection reason, and latest
published release. It contains no environment values, tokens, or KV contents.

Public provider facts derive the complete roster and optional presentation
labels from `crates/config/assets/provider_descriptors.json`, alongside the
catalog from the same selected source revision. Both local generation and the
remote drift check validate that descriptor document and refuse missing or
malformed data. Add metadata there; there is no separate website label map or
TUI enum roster to update.

## Visual direction

The public site is a documentation portal with a restrained underwater atmosphere. Content and navigation come first; ocean depth, currents, and the whale mark provide identity without turning every section into a themed card.

- **Palette**: cool paper and mist for reading surfaces, deep navy for terminal and community sections, muted current blue for links, and small gold/coral signals where status needs contrast.
- **Type**: Space Grotesk for headings, IBM Plex Sans for body copy, and JetBrains Mono for commands and compact interface labels.
- **Structure**: compact documentation rows, quiet hairline dividers, generous but bounded reading widths, and responsive layouts that remove chrome before content.

If you want to retune the palette, edit `:root` in `app/globals.css` and the `colors` block in `tailwind.config.ts`.
