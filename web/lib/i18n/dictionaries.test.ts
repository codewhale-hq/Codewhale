import { existsSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  DICTIONARY_LOCALES,
  EN_CHROME,
  EN_DOCS_GUIDE,
  EN_DOCS_HOOKS,
  EN_DOCS_MCP,
  EN_DOCS_RUNTIME_API,
  EN_DOCS_SANDBOX,
  EN_DOCS_SUBAGENTS,
  EN_DOCS_WEB,
  EN_DOCS_WORK,
  EN_DOCS_PET,
  EN_DOCS_AUTH,
  EN_DOCS_TRUST,
  EN_DOCS_FIRST_TIME,
  EN_DOCS_REVIEW,
  EN_DOCS_VOCABULARY,
  EN_COMPUTER_USE,
  EN_CHANGELOG,
  EN_DOCS_SHELL,
  EN_DOCS_TROUBLESHOOTING,
  EN_FAQ,
  EN_HOME,
  EN_LEGAL_PRIVACY,
  EN_LEGAL_TERMS,
  EN_CONTRIBUTE,
  EN_CONSTITUTION,
  fill,
  getChrome,
  getDocsGuide,
  getDocsHooks,
  getDocsMcp,
  getDocsRuntimeApi,
  getDocsSandbox,
  getDocsSubagents,
  getDocsWeb,
  getDocsWork,
  getDocsPet,
  getDocsAuth,
  getDocsTrust,
  getDocsFirstTime,
  getComputerUse,
  getChangelog,
  getDocsShell,
  getDocsTroubleshooting,
  getDocsConfiguration,
  getDocsFleet,
  getDocsModes,
  getDocsReview,
  getDocsVocabulary,
  getFaq,
  getHome,
  getLegalPrivacy,
  getLegalTerms,
  getContribute,
  getConstitution,
  pickText,
  pickTextLocale,
  splitToken,
} from "./dictionaries";
import { locales, partialLocales } from "./config";
import type { ChromeDict, HomeDict } from "./dictionaries/types";

/**
 * Keys whose value is a mark, a proper noun, or a formatting tag rather
 * than prose — a locale sharing English's value here is correct, not a
 * missing translation.
 */
const NON_PROSE_KEYS = new Set([
  "dateLocale",
]);

/** Chrome keys that are real sentences/labels and must be translated. */
const CHROME_PROSE_KEYS = [
  "skipToContent",
  "navDocs",
  "navSignIn",
  "translationMachineDraft",
  "navPrimaryAria",
  "navHomeAria",
  "menuOpen",
  "menuClose",
  "themeAria",
  "themeTitle",
  "footerProduct",
  "footerProject",
  "footerGuide",
  "footerCanonicalSource",
  "footerReleasesLink",
  "switcherLabel",
  "switcherSwitchTo",
  "partialBadge",
] as const satisfies readonly (keyof ChromeDict)[];

/**
 * Locale/key pairs whose English-identical value is a native loanword in
 * that locale, not a missing translation — asserted below so the equality
 * is deliberate and visible. German "Community" matches the TUI pack
 * (crates/tui/locales/de.json: "Community & Mitwirken").
 */
const CHROME_LOANWORDS: Record<string, readonly string[]> = {};

/** Home keys that are real sentences and must be translated. */
const HOME_PROSE_KEYS = [
  "metaTitle",
  "metaDescription",
  "heroTitle",
  "heroTitleLead",
  "heroIntro",
  "readTheCode",
  "installNote",
  "captionDevelopment",
  "captionReleased",
  "captureFrameNoPosture",
  "sectionModelHeading",
  "sectionModelBody",
  "sectionControlHeading",
  "sectionControlBody",
  "factRust",
  "factLicense",
  "sectionDirectionHeading",
  "sectionDirectionBody",
  "sectionOpenHeading",
  "sectionOpenBody",
  "sectionStartHeading",
  "sectionStartBody",
  "mapHeading",
  "mapStateReleased",
  "mapStateDeveloping",
  "mapStateInviteOnly",
  "mapStateBeingBuilt",
  "mapTerminal",
  "mapTerminalSentence",
  "mapLocalBrowser",
  "mapLocalBrowserSentence",
  "mapManyAgents",
  "mapManyAgentsSentence",
  "mapDesktop",
  "mapDesktopSentence",
  "mapCloud",
  "mapCloudSentence",
  "mapOneSession",
  "mapOneSessionSentence",
  "getCodewhale",
  "heroInstallAria",
  "shotPreview",
  "screenshotAlt",
  "frameSummaryPrompt",
  "frameSummaryEdit",
  "frameSummaryApproval",
  "frameSummaryResult",
  "frameSummaryProvider",
  "frameSummaryReceipt",
] as const satisfies readonly (keyof HomeDict)[];

function templateTokens(value: string): string[] {
  return [...value.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
}

function flattenStrings(dict: object): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(dict)) {
    if (typeof value === "string") {
      out[key] = value;
    } else if (Array.isArray(value)) {
      value.forEach((row: string | string[], i: number) => {
        if (typeof row === "string") {
          out[`${key}[${i}]`] = row;
        } else {
          row.forEach((cell, j) => {
            out[`${key}[${i}][${j}]`] = cell;
          });
        }
      });
    }
  }
  return out;
}

describe("website dictionaries", () => {
  it("cover every routed locale except the English reference", () => {
    expect([...DICTIONARY_LOCALES].sort()).toEqual(
      [
        "zh", "es", "id", "ja", "ko", "pt-BR", "ru", "uk", "vi",
        "fr", "de", "ca", "hi", "tr", "it", "pl", "ar",
      ].sort(),
    );
    // Chinese is dictionary-backed like every other locale — no inline
    // en/zh special case survives in the page/component sources (#4934).
    expect(DICTIONARY_LOCALES).toContain("zh");
    // Every routed locale either has its own dictionary or *is* English.
    for (const locale of locales) {
      expect(
        locale === "en" || DICTIONARY_LOCALES.includes(locale),
        `${locale} has no dictionary`,
      ).toBe(true);
    }
    // Every partial locale is dictionary-backed, so the partial badge marks
    // untranslated page bodies — never untranslated chrome.
    for (const locale of partialLocales) {
      expect(DICTIONARY_LOCALES, `${locale} partial pack`).toContain(locale);
    }
  });

  it("holds every dictionary to exact key parity with the English reference", () => {
    const enChromeKeys = Object.keys(EN_CHROME).sort();
    const enHomeKeys = Object.keys(EN_HOME).sort();
    for (const locale of DICTIONARY_LOCALES) {
      expect(Object.keys(getChrome(locale)).sort(), `${locale} chrome keys`).toEqual(
        enChromeKeys,
      );
      expect(Object.keys(getHome(locale)).sort(), `${locale} home keys`).toEqual(
        enHomeKeys,
      );
    }
  });

  it("preserves {token} template placeholders through translation", () => {
    const enChromeTokens = flattenStrings(EN_CHROME);
    const enHomeTokens = flattenStrings(EN_HOME);
    for (const locale of DICTIONARY_LOCALES) {
      const chrome = flattenStrings(getChrome(locale));
      const home = flattenStrings(getHome(locale));
      for (const key of Object.keys(enChromeTokens)) {
        expect(templateTokens(chrome[key]), `${locale} chrome ${key}`).toEqual(
          templateTokens(enChromeTokens[key]),
        );
      }
      for (const key of Object.keys(enHomeTokens)) {
        expect(templateTokens(home[key]), `${locale} home ${key}`).toEqual(
          templateTokens(enHomeTokens[key]),
        );
      }
    }
  });

  it("holds every shipped page dictionary to key parity and English fallback (#5337)", () => {
    const enKeys = Object.keys(EN_DOCS_GUIDE).sort();
    for (const locale of [...DICTIONARY_LOCALES, "fr", "und"]) {
      // Page dictionaries are optional per locale: whatever getDocsGuide
      // resolves — the locale's own file or the English fallback — must
      // carry the exact reference shape, so a page never sees a missing key.
      expect(Object.keys(getDocsGuide(locale)).sort(), `${locale} docs-guide keys`).toEqual(
        enKeys,
      );
    }
    // zh ships a real translation, not an English pass-through.
    expect(getDocsGuide("zh").overviewTitle).not.toBe(EN_DOCS_GUIDE.overviewTitle);
    // The wave-2 locales ship docs-guide too, translated rather than passed through.
    for (const locale of ["fr", "de", "ca", "hi", "tr", "it", "pl", "ar"]) {
      expect(getDocsGuide(locale).overviewTitle, `${locale} docs-guide`).not.toBe(
        EN_DOCS_GUIDE.overviewTitle,
      );
    }
    // A locale without the file falls back to the English reference object.
    expect(getDocsGuide("ja")).toBe(EN_DOCS_GUIDE);
  });

  it("holds the docs shell dictionary to the same contract (#5337)", () => {
    const enKeys = Object.keys(EN_DOCS_SHELL).sort();
    for (const locale of [...DICTIONARY_LOCALES, "fr", "und"]) {
      expect(Object.keys(getDocsShell(locale)).sort(), `${locale} docs-shell keys`).toEqual(
        enKeys,
      );
    }
    // zh ships a real translation, not an English pass-through.
    expect(getDocsShell("zh").heroTitle).not.toBe(EN_DOCS_SHELL.heroTitle);
    // Every other locale renders the English shell today, exactly as the
    // `isZh` ternaries in docs/layout.tsx did before the move.
    for (const locale of ["ja", "fr", "ar", "und"]) {
      expect(getDocsShell(locale), `${locale} docs-shell`).toBe(EN_DOCS_SHELL);
    }
  });

  it("holds the docs page-body dictionaries to the same contract (#5337)", () => {
    for (const [label, get, reference] of [
      ["docs-hooks", getDocsHooks, EN_DOCS_HOOKS],
      ["docs-troubleshooting", getDocsTroubleshooting, EN_DOCS_TROUBLESHOOTING],
      ["docs-runtime-api", getDocsRuntimeApi, EN_DOCS_RUNTIME_API],
      ["docs-sandbox", getDocsSandbox, EN_DOCS_SANDBOX],
      ["docs-subagents", getDocsSubagents, EN_DOCS_SUBAGENTS],
      ["docs-mcp", getDocsMcp, EN_DOCS_MCP],
      ["docs-web", getDocsWeb, EN_DOCS_WEB],
      ["docs-work", getDocsWork, EN_DOCS_WORK],
      ["docs-pet", getDocsPet, EN_DOCS_PET],
      ["docs-auth", getDocsAuth, EN_DOCS_AUTH],
      ["docs-trust", getDocsTrust, EN_DOCS_TRUST],
      ["docs-first-time", getDocsFirstTime, EN_DOCS_FIRST_TIME],
      ["docs-review", getDocsReview, EN_DOCS_REVIEW],
      ["docs-vocabulary", getDocsVocabulary, EN_DOCS_VOCABULARY],
      ["changelog", getChangelog, EN_CHANGELOG],
      ["legal-terms", getLegalTerms, EN_LEGAL_TERMS],
      ["legal-privacy", getLegalPrivacy, EN_LEGAL_PRIVACY],
      ["faq", getFaq, EN_FAQ],
      ["contribute", getContribute, EN_CONTRIBUTE],
      ["constitution", getConstitution, EN_CONSTITUTION],
    ] as const) {
      const enKeys = Object.keys(reference).sort();
      for (const locale of [...DICTIONARY_LOCALES, "fr", "und"]) {
        expect(Object.keys(get(locale)).sort(), `${locale} ${label} keys`).toEqual(enKeys);
      }
      // zh ships a real translation, not an English pass-through. The probe is
      // `metaTitle` rather than `overviewTitle` because docs/mcp's heading is
      // the code-owned literal `MCP` and stays in the page.
      expect(get("zh").metaTitle, `zh ${label}`).not.toBe(reference.metaTitle);
      // Contribute ships in every routed locale; the other page
      // dictionaries still render English outside en and zh.
      const translated = label === "contribute";
      for (const locale of ["ja", "fr", "ar"]) {
        if (translated) expect(get(locale).metaTitle, `${locale} ${label}`).not.toBe(reference.metaTitle);
        else expect(get(locale), `${locale} ${label}`).toBe(reference);
      }
      expect(get("und"), `und ${label}`).toBe(reference);
    }
  });

  it("ships the Computer Use page dictionary for every routed locale", () => {
    const enKeys = Object.keys(EN_COMPUTER_USE).sort();
    for (const locale of [...DICTIONARY_LOCALES, "und"]) {
      expect(Object.keys(getComputerUse(locale)).sort(), `${locale} computer-use keys`).toEqual(enKeys);
      expect(getComputerUse(locale).steps, `${locale} computer-use steps`).toHaveLength(3);
    }
    // The download page is translated for every routed locale, not passed
    // through: each dictionary locale resolves its own object with its own
    // primary-button label; only an unknown locale gets the English reference.
    for (const locale of DICTIONARY_LOCALES) {
      expect(getComputerUse(locale), `${locale} computer-use`).not.toBe(EN_COMPUTER_USE);
      expect(getComputerUse(locale).download, `${locale} computer-use download`).not.toBe(EN_COMPUTER_USE.download);
    }
    expect(getComputerUse("und")).toBe(EN_COMPUTER_USE);
  });

  it("keeps every docs task page aligned between English and Chinese", () => {
    const routeExists = (href: string) =>
      existsSync(new URL(`../../app/[locale]${href.split("#")[0]}/page.tsx`, import.meta.url));
    const internalLinks = (text: string) =>
      [...text.matchAll(/\]\((\/[^)\s]*)\)/g)].map((m) => m[1]);
    for (const [label, get] of [
      ["docs-auth", getDocsAuth],
      ["docs-configuration", getDocsConfiguration],
      ["docs-fleet", getDocsFleet],
      ["docs-hooks", getDocsHooks],
      ["docs-mcp", getDocsMcp],
      ["docs-modes", getDocsModes],
      ["docs-review", getDocsReview],
      ["docs-runtime-api", getDocsRuntimeApi],
      ["docs-sandbox", getDocsSandbox],
      ["docs-subagents", getDocsSubagents],
      ["docs-troubleshooting", getDocsTroubleshooting],
      ["docs-trust", getDocsTrust],
      ["docs-first-time", getDocsFirstTime],
      ["docs-web", getDocsWeb],
      ["docs-work", getDocsWork],
      ["docs-pet", getDocsPet],
    ] as const) {
      const en = get("en");
      const zh = get("zh");
      expect(zh, label).not.toBe(en);
      // Same sections, in the same order, with the same block shapes.
      expect(zh.sections.map((x) => x.id), label).toEqual(en.sections.map((x) => x.id));
      en.sections.forEach((section, i) => {
        const other = zh.sections[i].blocks;
        expect(other.map((b) => Object.keys(b)[0]), `${label}#${section.id}`).toEqual(
          section.blocks.map((b) => Object.keys(b)[0]),
        );
        section.blocks.forEach((block, j) => {
          const peer = other[j];
          // Commands are shown verbatim: a translation never edits one.
          if ("code" in block && "code" in peer) {
            expect(peer.code, `${label}#${section.id} code`).toBe(block.code);
          }
          if ("rows" in block && "rows" in peer) {
            expect(peer.rows.length, `${label}#${section.id} rows`).toBe(block.rows.length);
            if (block.codeTerms) {
              expect(peer.rows.map(([term]) => term)).toEqual(block.rows.map(([term]) => term));
            }
          }
          if ("steps" in block && "steps" in peer) {
            expect(peer.steps.length, `${label}#${section.id} steps`).toBe(block.steps.length);
          }
          if ("list" in block && "list" in peer) {
            expect(peer.list.length, `${label}#${section.id} list`).toBe(block.list.length);
          }
        });
      });
      expect(zh.next.map((n) => n.href), label).toEqual(en.next.map((n) => n.href));
      // Every internal link — inline or "next" — lands on a real page.
      for (const dict of [en, zh]) {
        const text = JSON.stringify(dict.sections);
        for (const href of [...internalLinks(text), ...dict.next.map((n) => n.href)]) {
          expect(routeExists(href), `${label}: ${href}`).toBe(true);
        }
      }
    }
  });

  it("pickText selects the locale side of legacy { en, zh } pairs", () => {
    const pair = { en: "English", zh: "中文" };
    expect(pickText(pair, "zh")).toBe("中文");
    expect(pickText(pair, "en")).toBe("English");
    expect(pickText(pair, "ja"), "non-zh locales read the English side").toBe("English");
    expect(pickTextLocale("zh")).toBe("zh");
    for (const locale of ["en", "ja", "ar"]) expect(pickTextLocale(locale)).toBe("en");
  });

  it("falls back to the English dictionary for unrouted locales — no missing markers", () => {
    for (const key of Object.keys(EN_CHROME) as (keyof ChromeDict)[]) {
      expect(getChrome("xx")[key]).toBe(EN_CHROME[key]);
      expect(getChrome("en")[key]).toBe(EN_CHROME[key]);
    }
    for (const key of Object.keys(EN_HOME) as (keyof HomeDict)[]) {
      expect(getHome("xx")[key]).toEqual(EN_HOME[key]);
    }
  });

  it("has no empty strings anywhere", () => {
    for (const locale of ["en", ...DICTIONARY_LOCALES]) {
      for (const [key, value] of Object.entries(flattenStrings(getChrome(locale)))) {
        expect(value.trim().length, `${locale} chrome ${key}`).toBeGreaterThan(0);
      }
      for (const [key, value] of Object.entries(flattenStrings(getHome(locale)))) {
        expect(value.trim().length, `${locale} home ${key}`).toBeGreaterThan(0);
      }
    }
  });

  it("keeps the Cyrillic packs script-pure (no cross-leakage, no mixed copy)", () => {
    const cyrillic = /[Ѐ-ӿ]/;
    for (const [key, value] of Object.entries(flattenStrings(getChrome("uk")))) {
      expect(value, `uk chrome ${key}`).not.toMatch(/[ыэъЫЭЪ]/);
      void cyrillic;
    }
    for (const [key, value] of Object.entries(flattenStrings(getHome("uk")))) {
      expect(value, `uk home ${key}`).not.toMatch(/[ыэъЫЭЪ]/);
    }
    for (const [key, value] of Object.entries(flattenStrings(getChrome("ru")))) {
      expect(value, `ru chrome ${key}`).not.toMatch(/[іІїЇєЄґҐ]/);
    }
    for (const [key, value] of Object.entries(flattenStrings(getHome("ru")))) {
      expect(value, `ru home ${key}`).not.toMatch(/[іІїЇєЄґҐ]/);
    }
    // Prose values are actually translated, not English pass-through.
    expect(getHome("ru").heroIntro).toMatch(cyrillic);
    expect(getHome("uk").heroIntro).toMatch(cyrillic);
    expect(getChrome("ru").navDocs).not.toBe(EN_CHROME.navDocs);
    expect(getChrome("uk").navDocs).not.toBe(EN_CHROME.navDocs);
    expect(getChrome("ru").navDocs).not.toBe(getChrome("uk").navDocs);
  });

  it("keeps the Chinese pack in Han script for prose (no English pass-through)", () => {
    const han = /[一-鿿]/;
    const chrome = getChrome("zh");
    const home = getHome("zh");
    for (const key of CHROME_PROSE_KEYS) {
      expect(chrome[key], `zh chrome ${key}`).toMatch(han);
    }
    for (const key of HOME_PROSE_KEYS) {
      expect(home[key], `zh home ${key}`).toMatch(han);
    }
    // Chinese resolves to its OWN dictionary, not the English reference.
    expect(chrome.navDocs).not.toBe(EN_CHROME.navDocs);
    expect(home.heroTitle).not.toBe(EN_HOME.heroTitle);
  });

  it("leaves no unmarked English prose in any non-English dictionary", () => {
    for (const locale of DICTIONARY_LOCALES) {
      const chrome = getChrome(locale);
      const home = getHome(locale);
      const loanwords = new Set(CHROME_LOANWORDS[locale] ?? []);
      for (const key of CHROME_PROSE_KEYS) {
        if (loanwords.has(key)) {
          // A documented loanword: the shared value IS the native word.
          expect(chrome[key], `${locale} chrome ${key} loanword`).toBe(EN_CHROME[key]);
          continue;
        }
        expect(chrome[key], `${locale} chrome ${key} is English pass-through`).not.toBe(
          EN_CHROME[key],
        );
      }
      for (const key of HOME_PROSE_KEYS) {
        expect(home[key], `${locale} home ${key} is English pass-through`).not.toBe(
          EN_HOME[key],
        );
      }
    }
  });

  it("keeps marks, tags, and proper nouns out of the translated-prose rule", () => {
    // Documents the deliberate exceptions so a future audit does not read a
    // shared value here as a missing translation.
    for (const key of NON_PROSE_KEYS) {
      const inChrome = key in EN_CHROME;
      const inHome = key in EN_HOME;
      expect(inChrome || inHome, `${key} is not a real dictionary key`).toBe(true);
      expect(CHROME_PROSE_KEYS as readonly string[]).not.toContain(key);
      expect(HOME_PROSE_KEYS as readonly string[]).not.toContain(key);
    }
  });

  it("carries the {brand} token through every hero lede for splitToken()", () => {
    for (const locale of ["en", ...DICTIONARY_LOCALES]) {
      const lede = getHome(locale).heroIntro;
      expect(lede, `${locale} heroIntro`).toContain("{brand}");
      const parts = splitToken(lede, "brand");
      expect(parts.length, `${locale} heroIntro brand split`).toBe(2);
      expect(parts.join("").includes("{brand}")).toBe(false);
    }
  });

  it("starts every headline with its lead sentence so a page can set the two sentences apart", () => {
    for (const locale of ["en", ...DICTIONARY_LOCALES]) {
      const home = getHome(locale);
      expect(home.heroTitle.startsWith(home.heroTitleLead), `${locale} heroTitle`).toBe(true);
      expect(home.heroTitle.length, `${locale} heroTitle has a second sentence`).toBeGreaterThan(home.heroTitleLead.length);
      expect(home.metaTitle, `${locale} metaTitle`).toBe(`Codewhale · ${home.heroTitle}`);
    }
  });

  it("carries {date} through both legal pages' effective-date line", () => {
    // The date used to sit in the JSX between two translated fragments. Now
    // it is a fill() token, so a translation that drops it would ship a legal
    // page that no longer says when it took effect.
    for (const locale of ["en", "zh"]) {
      expect(getLegalTerms(locale).updated, `${locale} legal-terms`).toContain("{date}");
      expect(getLegalPrivacy(locale).updated, `${locale} legal-privacy`).toContain("{date}");
    }
  });

  it("interpolates templates with fill() and leaves unknown tokens visible", () => {
    expect(fill("Latest release {tag}", { tag: "v0.9.2" })).toBe("Latest release v0.9.2");
    expect(fill("{count} provider routes", { count: 30 })).toBe("30 provider routes");
    expect(fill("v{version} {state}", { version: "0.9.2" })).toBe("v0.9.2 {state}");
  });
});
