# Localization Matrix

> 阅读简体中文版：[zh_hans/LOCALIZATION.md](zh_hans/LOCALIZATION.md)。

Canonical tracking document for every locale Codewhale ships, is actively
building, is planning, or has explicitly deferred.

> **Scope note (2026-07-12):** this matrix covers three surfaces — the TUI
> locale packs (`crates/localization/locales/`), the translated READMEs (repo root),
> and the website (`web/`). The three ship on different cadences, so a
> locale can be **shipped** on one surface and **planned** on another; the
> per-surface tables below are the per-surface truth. The website registry
> is `web/lib/i18n/config.ts` (`ALL_LOCALES`): the locale switcher and route
> generation both derive from it.
>
> Docs translations are **not** a locale surface: they live under
> `docs/zh_hans/` and `docs/id/`, and their status is tracked in
> `docs/zh_hans/README.md` and issue #5482, not in this matrix.

Customer-visible copy also follows the [Codewhale voice and terminal
charter](VOICE.md); commands, key names, and glyphs remain code-owned around
localized prose.

Last updated: 2026-08-18 (docs/zh_hans/ restructure; docs translation
status tracked outside this matrix, per #5482).
Source-of-truth README: `README.md` (English, post-#3087).

## Status legend

| Status | Meaning |
|--------|---------|
| **shipped** | Live on codewhale.net and/or published as a standalone README, or a TUI pack at exact `en.json` parity |
| **partial** | Shipped but intentionally incomplete; missing scope falls back to English and the partial status is visible |
| **planned** | Explicitly prioritized for the next wave |
| **deferred** | Acknowledged as wanted but not yet scheduled; needs layout QA, bridge support, or community champion |

---

## TUI locale packs

The TUI packs under `crates/localization/locales/` are the largest translation
surface in the repo. `en.json` is the reference; a pack is **complete**
only at exact raw key parity with it, enforced by
`scripts/check-tui-locale-parity.py` (CI) and the parity tests in
`crates/localization/src/lib.rs`. See `crates/localization/locales/AGENTS.md` for the
authoring contract.

| Locale | File | Keys vs `en.json` | Status | Notes |
|--------|------|--------------------------|--------|-------|
| English | `en.json` | all | **shipped** | Reference pack. |
| Japanese | `ja.json` | all | **shipped** | Complete. |
| Simplified Chinese | `zh-Hans.json` | all | **shipped** | Complete. |
| Traditional Chinese | `zh-Hant.json` | all | **shipped** | Complete (#5143). Awaiting native-speaker review. |
| Brazilian Portuguese | `pt-BR.json` | all | **shipped** | Complete. |
| Latin American Spanish | `es-419.json` | all | **shipped** | Complete. Note the website tracks `es` — the shipped TUI pack is Latin American Spanish, not `es-ES`. |
| Vietnamese | `vi.json` | all | **shipped** | Complete. |
| Korean | `ko.json` | all | **shipped** | Complete. |
| Catalan | `ca.json` | all | **shipped** | Complete (#4749/#4788). Awaiting native-speaker review. |
| German | `de.json` | all | **shipped** | Complete (#4788). Awaiting native-speaker review. |
| French | `fr.json` | all | **shipped** | Complete (#4788). Awaiting native-speaker review. |
| Indonesian | `id.json` | all | **shipped** | Complete (#4789). Awaiting native-speaker review. |
| Hindi | `hi.json` | all | **shipped** | Complete (#4790). The Devanagari shaping spike (moved out of this repository in `7242381022`) gave code-level guarantees only; terminal visual QA and native review still open. |
| Russian | `ru.json` | all | **shipped** | Complete (#3092). Cyrillic script fixtures guard against mixed-language copy. Awaiting native-speaker review. |
| Ukrainian | `uk.json` | all | **shipped** | Complete (#4791). Cyrillic script fixtures keep it distinct from Russian (no ы/э/ъ; і/ї/є/ґ present). Awaiting native-speaker review. |

## Website locales

The website derives routing, the switcher, sitemap, and hreflang from
`ALL_LOCALES` in `web/lib/i18n/config.ts` — one canonical registry, no
second taxonomy. **partial** locales route and are selectable with a
visible `(partial)` badge in the switcher; their dictionaries
(`web/lib/i18n/dictionaries/<code>/`) cover shared chrome (masthead, nav,
mobile menu, theme toggle, live ticker, footer, switcher) and the home page,
held to exact key parity with the English reference by
`npm run check:locales` and `web/lib/i18n/dictionaries.test.ts`.
Everything outside that scope renders the English page copy — a deliberate
fallback, never a dictionary key on screen.

**As of #4934 (v0.9.4) there is one dictionary path for every routed
locale, Chinese included.** `web/app/[locale]/page.tsx`,
`web/components/nav.tsx`, and `web/components/footer.tsx` no longer carry an
`isZh` / `foreign` copy branch: they read `getHome(locale)` and
`getChrome(locale)`. `web/lib/i18n/dictionaries/zh/` now exists (it used to
be inline TSX), and nav/footer link sets are generated once in
`web/lib/i18n/links.ts` so every locale gets the identical route shape.

**Website/docs translation pipeline (General Translation CLI, 2026-08-28).**
Runtime stays the dictionaries above — do not add `gt-next` beside them.
`web/gt-catalog/[locale].json` is the local JSON interchange (`en` + live
`zh` first). `npm run i18n:gt -- export` writes catalogs from dictionaries;
`check` (hooked from `check:locales`) requires them to match; `import`
writes reviewed JSON back to website dictionary TS only. `translate` is
fail-closed unless BYOK `GT_API_KEY` and `GT_PROJECT_ID` are set in the
environment — never commit those values, never point this config at
`crates/localization/locales`, and never wrap model completions. `gt generate` is
not used: it is a framework JSX scanner, not a JSON-catalog tool.
Reference shape: **`ChromeDict` 52 keys, `HomeDict` 62 keys.** Bilingual
secondary nav labels, the masthead seal and issue line, the ticker live
label, and the per-locale `Intl` date tag are dictionary values — no locale
renders another language's script by accident.

| Locale | Code | Status | Notes |
|--------|------|--------|-------|
| English | `en` | **shipped** | Source text and the reference dictionary shape. Every page has an EN route. |
| Simplified Chinese | `zh` | **shipped** | Full parity with EN on all first-class pages. Chrome + home are dictionary-backed (`dictionaries/zh/`) as of #4934; the remaining page bodies are still inline `{ en, zh }` content modules. |
| Japanese | `ja` | **partial** | #3091. Chrome + home page localized via dictionary; other page bodies/metadata fall back to English. |
| Vietnamese | `vi` | **partial** | #3091. Same scope as Japanese. |
| Korean | `ko` | **partial** | #3093. Same scope as Japanese. |
| Russian | `ru` | **partial** | #3092. Same scope as Japanese. |
| Ukrainian | `uk` | **partial** | #4791 — shipped alongside Russian, same scope. |
| Spanish | `es` | **partial** | #3093. Same scope as Japanese. |
| Brazilian Portuguese | `pt-BR` | **partial** | #3093. Same scope as Japanese. |
| French | `fr` | **planned** | #4788 — TUI pack shipped in v0.9.2; website next wave. |
| German | `de` | **planned** | #4788 — TUI pack shipped in v0.9.2; website next wave. |
| Catalan | `ca` | **planned** | #4749/#4788 — TUI pack shipped in v0.9.2; website next wave. |
| Indonesian | `id` | **partial** | #4789. Same scope as Japanese. |
| Hindi | `hi` | **planned** | #4790 — TUI pack shipped in v0.9.2; website next wave. |
| Arabic | `ar` | **deferred** | RTL candidate. Deferred until layout/typography QA exists (bidirectional text, mirrored chrome, number formatting). |

Every partial locale carries the full 52/62 key set (see
`npm run check:locales`); the chrome and home page are genuinely translated,
not English pass-through — `dictionaries.test.ts` fails on an English
prose value in a non-English pack. The new v0.9.4 strings are
machine-translated to the same standard as the rest of each pack and are
**awaiting native-speaker review**, consistent with the TUI packs above.

Remaining website scope for the partial locales (next wave): per-page body
copy and `generateMetadata` titles/descriptions beyond the home page, the
`{ en, zh }` shared-content modules under `web/lib/content/`, the
TerminalPlayer scene excerpts in `web/components/thinking-trace.tsx`, and
the `KIND_LABEL` pairs in `web/components/feed-card.tsx`. The dictionary
layer, routing, hreflang, and switcher already cover them, so filling in a
page is a dictionary edit, not plumbing. That remaining English is exactly
what the `(partial)` badge is honest about.

## README locales

| Locale | File | Status | Parity check |
|--------|------|--------|-------------|
| English | `README.md` | **shipped** | Canonical source |
| Simplified Chinese | `README.zh-CN.md` | **shipped** | `scripts/check-readme-translations.py` (stamp + fences + URLs + sections) |
| Japanese | `README.ja-JP.md` | **shipped** | Same |
| Vietnamese | `README.vi.md` | **shipped** | Same |
| Korean | `README.ko-KR.md` | **shipped** | Same |
| Latin American Spanish | `README.es-419.md` | **shipped** | Same |
| Brazilian Portuguese | `README.pt-BR.md` | **shipped** | Same |
| Russian | `README.ru.md` | **shipped** | Same (#3092). Awaiting native-speaker review. |
| Ukrainian | `README.uk.md` | **shipped** | Same (#4791). Awaiting native-speaker review. |
| Indonesian | `README.id.md` | **shipped** | Same (#4789). Awaiting native-speaker review. |
| French | `README.fr.md` | **shipped** | Same. Awaiting native-speaker review. |
| German | `README.de.md` | **shipped** | Same. Awaiting native-speaker review. |
| Traditional Chinese | `README.zh-TW.md` | **shipped** | Same. Awaiting native-speaker review. |
| Hindi | `README.hi.md` | **shipped** | Same. Awaiting native-speaker review. |
| Turkish | `README.tr.md` | **shipped** | Same. Awaiting native-speaker review. |
| Italian | `README.it.md` | **shipped** | Same. Awaiting native-speaker review. |
| Polish | `README.pl.md` | **shipped** | Same. Awaiting native-speaker review. |
| Arabic | `README.ar.md` | **shipped** | Same. Awaiting native-speaker review. Markdown only; no HTML `dir` attributes. |
| Catalan | `README.ca.md` | **shipped** | Same. Awaiting native-speaker review. |

## Drift checks

| Check | Tool | Status |
|-------|------|--------|
| TUI pack key parity with `en.json` (complete packs) | `scripts/check-tui-locale-parity.py` + parity tests in `crates/localization/src/lib.rs` | **Shipped** (CI Lint job) |
| README translations stay in sync with `README.md` | `scripts/check-readme-translations.py` | **Shipped** (CI Lint job) |
| README locale links symmetric | `scripts/check-readme-locales.sh` | **Shipped** (CI Lint job) |
| Website dictionaries cover every routed locale except the `en` reference | `npm run check:locales` + `web/lib/i18n/dictionaries.test.ts` | **Shipped** (#3091, extended to `zh` in #4934) |
| No unmarked English prose survives in a non-English website dictionary | `leaves no unmarked English prose in any non-English dictionary` in `web/lib/i18n/dictionaries.test.ts` | **Shipped** (#4934) |
| Nav/footer routes stay in locale-swap parity for every routed locale | `web/lib/docs-ia.test.ts` over `web/lib/i18n/links.ts` | **Shipped** (#4934) |
| Accept-Language routes deterministically to all routed locales | `web/lib/i18n/detect.test.ts` (middleware delegates to `lib/i18n/detect.ts`) | **Shipped** (#3091) |
| Locale selector lists all routed locales with partial badges | `web/lib/i18n/config.test.ts` (switcher + router derive from one registry) | **Shipped** (#3091) |
| hreflang alternates cover every routed locale | `web/lib/page-meta.test.ts` | **Shipped** (#3091) |
| Cyrillic packs stay script-pure (no mixed-language copy, ru≠uk) | `cyrillic_packs_have_script_purity_and_no_mixed_language_fixtures` in `crates/localization/src/lib.rs` + `dictionaries.test.ts` | **Shipped** (#3092/#4791) |
| Devanagari grapheme-safe clip/wrap at 40/60/80 columns | `truncate_to_width_never_splits_devanagari_clusters` + width fixtures in `crates/localization/src/lib.rs` | **Shipped** (#4790) |
| Adding a UI locale never changes model-visible prompt bytes | `v092_locales_add_no_prompt_bookends_so_prompt_bytes_stay_stable` in `crates/tui/src/prompts.rs` | **Shipped** (cache-stability contract) |
| No shipped locale renders a missing-message marker | `no_shipped_locale_renders_a_missing_message_marker` in `crates/localization/src/lib.rs` | **Shipped** |

## How to add a locale

A locale is not "added" until all three surfaces below either ship it or
carry an explicit `planned`/`partial`/`deferred` row in this matrix.

### 1. TUI pack

1. Create `crates/localization/locales/<tag>.json` with every key in `en.json`,
   following `crates/localization/locales/AGENTS.md` (placeholders stay literal;
   product terms stay English per pack convention; preserve intentional
   leading/trailing spaces).
2. Add the `Locale` variant plus its `tag`/`translation_target_name`/
   `parse_locale`/`shipped`/`shipped_complete` arms in
   `crates/localization/src/lib.rs`, and the `include_str!` arm in the
   test module.
3. Wire the surfaces that still enumerate locales by hand. The `locale`
   setting is a plain string row in `crates/config/src/settings_schema.rs`,
   validated by `normalize_configured_locale` (which reuses `parse_locale`
   from step 2), and the `/config` value list (`config_choice_values` in
   `crates/tui/src/tui/views/mod.rs`) and hint text
   (`configured_locale_values`) derive from `Locale::shipped()`, so those need
   no edit. Review the setup wizard (`crates/tui/src/tui/setup/mod.rs`) and
   `locale_display` in `crates/tui/src/commands/groups/config/config.rs` for
   explicit locale mappings. Also review `public_site_locale_segment` in
   `crates/tui/src/commands/groups/core/links.rs`: the portable `/links` command
   matches locale tags, not the host `Locale` enum. Unlisted tags fall back to
   English website pages, so the compiler will not flag a newly added locale
   here. Add an explicit tag mapping when that locale is available on the website,
   and verify `/links` selects the intended documentation/community URLs.
   Add a `LANGUAGE_OPTIONS`
   entry to the onboarding language picker
   (`crates/tui/src/tui/onboarding/language.rs`); its
   `picker_offers_every_shipped_locale` test fails until you do. Several no-English-leak tests
   (for example in `status_picker.rs` and `tool_card.rs`) list locales
   explicitly; add the new tag there when the pack is complete.
4. Run `python3 scripts/check-tui-locale-parity.py` and
   `cargo test -p codewhale-tui localization`.
5. If the pack must ship incomplete, declare it partial: keep it out of
   `shipped_complete()`, mark it in `is_partial_pack()`, and add it to
   `PARTIAL_PACKS` in `scripts/check-tui-locale-parity.py` with a tracking
   issue. No pack is partial today — `PARTIAL_PACKS` is empty and
   `is_partial_pack()` returns false for every shipped locale — so a new
   entry is the only thing that reopens the English-fallback path.

### 2. README

1. Translate `README.md` into `README.<tag>.md`, preserving structure,
   commands, and the #3087 factual history.
2. Cross-link it from the language line in `README.md` and from the other
   translated READMEs.
3. Restamp per `scripts/check-readme-translations.py`, then run
   `python3 scripts/check-readme-translations.py` and
   `bash scripts/check-readme-locales.sh`.

### 3. Website

1. Add/flip the locale entry in `ALL_LOCALES` in `web/lib/i18n/config.ts` —
   the switcher, routes, middleware, sitemap, and hreflang derive from it,
   so no per-locale switcher edit is needed. Use the `partial` status for
   locales that ship the chrome+home dictionary scope before full page
   parity.
2. Create `web/lib/i18n/dictionaries/<code>/chrome.ts` and `home.ts`
   following the English reference shape (`dictionaries/en/`).
3. Middleware detection needs no change for base tags; region variants and
   base→variant mappings live in `web/lib/i18n/detect.ts`.
4. Run `cd web && npm run check:locales && npm test && npm run build`.

### 4. Matrix

Update the TUI, README, and Website tables above — one row per surface,
with per-surface status.

## Assessments

### Galician (`gl`) and Basque (`eu`) — 2026-07-25, per #4749

Assessed alongside the Catalan pack (#4749 / #4788), which asked whether
Galician and Basque are "similar-value European additions" worth shipping
in the same wave.

**Decision: defer both.** Rationale:

- The case #4788 makes for Catalan is specifically that it "has an
  unusually strong software-localization tradition and an active volunteer
  community" — a review-capacity argument, not a market-size one. That
  argument does not transfer: Galician and Basque have materially smaller
  localization communities, so a pack for either would ship with no
  realistic path to native-speaker review.
- Galician speakers have a workable fallback already: the shipped
  `es-419` pack (and `pt-BR` is lexically close). Basque is a language
  isolate with no fallback proximity — its per-string review cost is the
  highest of the three, and machine-translated Basque is the least
  trustworthy of the three.
- There is no natural "ship together" grouping: the v0.9.2 wave already
  bundles the locales that share acceptance criteria (Latin-script
  fr/de/ca/id, Cyrillic uk, Devanagari hi). gl/eu share only the
  review-capacity constraint, which neither clears.

**Cost/demand evidence behind the decision:** a complete TUI pack is
1,299 keys (~8–12k words) plus an ongoing obligation to retranslate every
changed English string in lockstep — the parity gate makes silent drift a
CI failure, so an unmaintained pack is worse than none. No community
member has requested gl or eu (no issues, no PRs, no translations offered),
while the gl/eu base tags already route cleanly through
`web/middleware.ts` the day a champion appears. We do not ship packs we
cannot get natively reviewed, and we do not advertise unshipped packs.

Revisit when a native-speaker champion appears for either language, or if
Catalan uptake after v0.9.2 suggests demand. Both base tags (`gl`, `eu`)
route through `web/middleware.ts` with no middleware change when that
happens.

## Related issues

- #3091 — Website parity with JA + VI README locales
- #3092 — Russian README + website localization
- #3093 — Korean, Spanish, Brazilian Portuguese next-wave locales
- #3087 — Post-rebrand README source text refresh
- #4057 — `zh-Hant` scoped as a partial TUI pack with English fallback
- #4787 — This matrix's TUI table + the locale-drift CI gates
- #4788 — French, German, Catalan TUI localization
- #4789 — Indonesian localization
- #4790 — Hindi localization + Devanagari terminal-shaping spike
- #4791 — Ukrainian localization alongside Russian
- #4749 — Catalan UI language + Galician/Basque assessment
- #5482 — EPIC(docs): review, partially restructure, and fully localize
  documentation to Chinese
