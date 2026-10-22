/**
 * release-credits.ts — community credit data for the website.
 *
 * CANONICAL SOURCE: docs/CONTRIBUTORS.md
 *   That file is the full per-PR contributor record in chronological order.
 *   This module is a snapshot of the latest release's merged/harvested
 *   contributors and issue helpers, kept in sync by the release process.
 *   When cutting a release, update both docs/CONTRIBUTORS.md AND this file.
 *
 * EXTENSION PATH FOR NEW LOCALES:
 *   The arrays below are locale-agnostic (GitHub handles). Locale-specific
 *   section labels and descriptions live in the page component. To add a
 *   new locale, update the page copy — no data changes needed here.
 *
 * See also:
 *   - .github/AUTHOR_MAP for identity mapping
 *   - CHANGELOG.md for the full release narrative
 *   - https://github.com/codewhale-hq/CodeWhale/graphs/contributors for the live list
 */

/** Contributors whose PRs were merged or harvested into this release. */
export const RELEASE_CONTRIBUTORS: string[] = [
  "@shenjackyuanjie",
  "@aboimpinto",
  "@printf033",
  "@henrikdukefoss",
  "@SparkofSpike",
  "@dajiaohuang",
  "@gaord",
  "@Lstarsky0",
  "@jayanthvee",
  "@leoca",
  "@asto18089",
  "@LIghtJUNction",
  "@zhuowp",
];

/**
 * Contributors whose work landed after the latest release and before the next
 * one. scripts/check-contributor-credit.py requires them here now; the release
 * cut moves them into RELEASE_CONTRIBUTORS with that release's changelog block.
 */
export const UNRELEASED_CONTRIBUTORS: string[] = [
  "@aboimpinto",
  "@AdityaVG13",
  "@printf033",
];

/**
 * Contributors who helped with reports, reproductions, and verification.
 * Credit covers the 0.10.2 reports recorded in docs/CONTRIBUTORS.md.
 */
export const RELEASE_HELPERS: string[] = [
  "@BX166",
  "@harryvizcaino-oss",
  "@rafacouto",
  "@luestr",
  "@SergiuSSoluS",
  "@8sero",
  "@Asteria1028",
  "@SakuraNeko",
  "@Statter",
  "@7jrxt42BxFZo4iAnN4CX",
];
