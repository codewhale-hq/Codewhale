import { docsClaudeCode as enDocsClaudeCode } from "./en/docs-claude-code";
import { docsClaudeCode as zhDocsClaudeCode } from "./zh/docs-claude-code";
import { enterprise as enEnterprise, type EnterpriseDict } from "./en/enterprise";
import { enterprise as zhEnterprise } from "./zh/enterprise";
import { docsSkills as enDocsSkills } from "./en/docs-skills";
import { docsSkills as zhDocsSkills } from "./zh/docs-skills";
/**
 * Dictionary loader for the website localization layer (#3091, #4934).
 *
 * Lookup is deterministic: a routed locale with a dictionary gets its own
 * copy; every other locale gets the English reference dictionary. There is
 * no per-key fallback chain — each shipped/partial dictionary is held to
 * exact key parity with English by `web/scripts/check-locales.mjs` and
 * `dictionaries.test.ts`, so a missing key is a build-time failure, never
 * a runtime "missing marker".
 *
 * English resolves through the `?? enChrome` / `?? enHome` fallback rather
 * than a map entry, which keeps `DICTIONARY_LOCALES` equal to the set of
 * non-reference locale directories that `check-locales.mjs` walks.
 */
import type {
  ChangelogDict,
  ChromeDict,
  ContributeDict,
  ConstitutionDict,
  RuntimeDict,
  ComputerUseDict,
  DocsAuthDict,
  DocsConfigurationDict,
  DocsFleetDict,
  DocsGuideDict,
  DocsHooksDict,
  DocsMcpDict,
  DocsRuntimeApiDict,
  DocsSandboxDict,
  DocsShellDict,
  DocsModesDict,
  DocsReviewDict,
  DocsVocabularyDict,
  DocsSubagentsDict,
  DocsTroubleshootingDict,
  DocsTrustDict,
  DocsWebDict,
  DocsWorkDict,
  DocsPetDict,
  HomeDict,
  FeaturesDict,
  DocsPageDict,
  DigestDict,
  FeedDict,
  CommunityDict,
  FaqDict,
  LegalPrivacyDict,
  LegalTermsDict,
  RoadmapDict,
  StatesDict,
} from "./types";
import { features as features0 } from "./en/features";
import { features as features1 } from "./zh/features";
import { features as features2 } from "./ja/features";
import { features as features3 } from "./vi/features";
import { features as features4 } from "./ko/features";
import { features as features5 } from "./ru/features";
import { features as features6 } from "./uk/features";
import { features as features7 } from "./es/features";
import { features as features8 } from "./pt-BR/features";
import { features as features9 } from "./fr/features";
import { features as features10 } from "./de/features";
import { features as features11 } from "./ca/features";
import { features as features12 } from "./id/features";
import { features as features13 } from "./hi/features";
import { features as features14 } from "./tr/features";
import { features as features15 } from "./it/features";
import { features as features16 } from "./pl/features";
import { features as features17 } from "./ar/features";
import { chrome as enChrome } from "./en/chrome";
import { home as enHome } from "./en/home";
import { docsGuide as enDocsGuide } from "./en/docs-guide";
import { docsGuide as zhDocsGuide } from "./zh/docs-guide";
import { docsShell as enDocsShell } from "./en/docs-shell";
import { docsShell as zhDocsShell } from "./zh/docs-shell";
import { docsHooks as enDocsHooks } from "./en/docs-hooks";
import { docsHooks as zhDocsHooks } from "./zh/docs-hooks";
import { docsTroubleshooting as enDocsTroubleshooting } from "./en/docs-troubleshooting";
import { docsTroubleshooting as zhDocsTroubleshooting } from "./zh/docs-troubleshooting";
import { docsConfiguration as enDocsConfiguration } from "./en/docs-configuration";
import { docsConfiguration as zhDocsConfiguration } from "./zh/docs-configuration";
import { docsFleet as enDocsFleet } from "./en/docs-fleet";
import { docsFleet as zhDocsFleet } from "./zh/docs-fleet";
import { docsMcp as enDocsMcp } from "./en/docs-mcp";
import { docsMcp as zhDocsMcp } from "./zh/docs-mcp";
import { docsModes as enDocsModes } from "./en/docs-modes";
import { docsModes as zhDocsModes } from "./zh/docs-modes";
import { docsRuntimeApi as enDocsRuntimeApi } from "./en/docs-runtime-api";
import { docsRuntimeApi as zhDocsRuntimeApi } from "./zh/docs-runtime-api";
import { docsSandbox as enDocsSandbox } from "./en/docs-sandbox";
import { docsSandbox as zhDocsSandbox } from "./zh/docs-sandbox";
import { docsSubagents as enDocsSubagents } from "./en/docs-subagents";
import { docsSubagents as zhDocsSubagents } from "./zh/docs-subagents";
import { docsWeb as enDocsWeb } from "./en/docs-web";
import { docsWeb as zhDocsWeb } from "./zh/docs-web";
import { docsWork as enDocsWork } from "./en/docs-work";
import { docsWork as zhDocsWork } from "./zh/docs-work";
import { docsPet as enDocsPet } from "./en/docs-pet";
import { docsPet as zhDocsPet } from "./zh/docs-pet";
import { docsAuth as enDocsAuth } from "./en/docs-auth";
import { docsAuth as zhDocsAuth } from "./zh/docs-auth";
import { docsTrust as enDocsTrust } from "./en/docs-trust";
import { docsTrust as zhDocsTrust } from "./zh/docs-trust";
import { docsReview as enDocsReview } from "./en/docs-review";
import { docsReview as zhDocsReview } from "./zh/docs-review";
import { docsVocabulary as enDocsVocabulary } from "./en/docs-vocabulary";
import { docsVocabulary as zhDocsVocabulary } from "./zh/docs-vocabulary";
import { computerUse as enComputerUse } from "./en/computer-use";
import { computerUse as zhComputerUse } from "./zh/computer-use";
import { computerUse as jaComputerUse } from "./ja/computer-use";
import { computerUse as viComputerUse } from "./vi/computer-use";
import { computerUse as koComputerUse } from "./ko/computer-use";
import { computerUse as ruComputerUse } from "./ru/computer-use";
import { computerUse as ukComputerUse } from "./uk/computer-use";
import { computerUse as esComputerUse } from "./es/computer-use";
import { computerUse as frComputerUse } from "./fr/computer-use";
import { computerUse as deComputerUse } from "./de/computer-use";
import { computerUse as caComputerUse } from "./ca/computer-use";
import { computerUse as hiComputerUse } from "./hi/computer-use";
import { computerUse as trComputerUse } from "./tr/computer-use";
import { computerUse as itComputerUse } from "./it/computer-use";
import { computerUse as plComputerUse } from "./pl/computer-use";
import { computerUse as arComputerUse } from "./ar/computer-use";
import { computerUse as ptBrComputerUse } from "./pt-BR/computer-use";
import { computerUse as idComputerUse } from "./id/computer-use";
import { states as enStates } from "./en/states";
import { states as zhStates } from "./zh/states";
import { changelog as enChangelog } from "./en/changelog";
import { changelog as zhChangelog } from "./zh/changelog";
import { legalTerms as enLegalTerms } from "./en/legal-terms";
import { legalTerms as zhLegalTerms } from "./zh/legal-terms";
import { legalPrivacy as enLegalPrivacy } from "./en/legal-privacy";
import { legalPrivacy as zhLegalPrivacy } from "./zh/legal-privacy";
import { digest as enDigest } from "./en/digest";
import { digest as zhDigest } from "./zh/digest";
import { feed as enFeed } from "./en/feed";
import { feed as zhFeed } from "./zh/feed";
import { community as enCommunity } from "./en/community";
import { community as zhCommunity } from "./zh/community";
import { faq as enFaq } from "./en/faq";
import { faq as zhFaq } from "./zh/faq";
import { roadmap as enRoadmap } from "./en/roadmap";
import { roadmap as zhRoadmap } from "./zh/roadmap";
import { contribute as enContribute } from "./en/contribute";
import { contribute as zhContribute } from "./zh/contribute";
import { constitution as enConstitution } from "./en/constitution";
import { constitution as zhConstitution } from "./zh/constitution";
import { runtime as enRuntime } from "./en/runtime";
import { runtime as zhRuntime } from "./zh/runtime";
import { chrome as zhChrome } from "./zh/chrome";
import { home as zhHome } from "./zh/home";
import { chrome as jaChrome } from "./ja/chrome";
import { home as jaHome } from "./ja/home";
import { chrome as viChrome } from "./vi/chrome";
import { home as viHome } from "./vi/home";
import { chrome as koChrome } from "./ko/chrome";
import { home as koHome } from "./ko/home";
import { chrome as ruChrome } from "./ru/chrome";
import { home as ruHome } from "./ru/home";
import { chrome as ukChrome } from "./uk/chrome";
import { home as ukHome } from "./uk/home";
import { chrome as esChrome } from "./es/chrome";
import { home as esHome } from "./es/home";
import { chrome as frChrome } from "./fr/chrome";
import { home as frHome } from "./fr/home";
import { docsGuide as frDocsGuide } from "./fr/docs-guide";
import { chrome as deChrome } from "./de/chrome";
import { home as deHome } from "./de/home";
import { docsGuide as deDocsGuide } from "./de/docs-guide";
import { chrome as caChrome } from "./ca/chrome";
import { home as caHome } from "./ca/home";
import { docsGuide as caDocsGuide } from "./ca/docs-guide";
import { chrome as hiChrome } from "./hi/chrome";
import { home as hiHome } from "./hi/home";
import { docsGuide as hiDocsGuide } from "./hi/docs-guide";
import { chrome as trChrome } from "./tr/chrome";
import { home as trHome } from "./tr/home";
import { docsGuide as trDocsGuide } from "./tr/docs-guide";
import { chrome as itChrome } from "./it/chrome";
import { home as itHome } from "./it/home";
import { docsGuide as itDocsGuide } from "./it/docs-guide";
import { chrome as plChrome } from "./pl/chrome";
import { home as plHome } from "./pl/home";
import { docsGuide as plDocsGuide } from "./pl/docs-guide";
import { chrome as arChrome } from "./ar/chrome";
import { home as arHome } from "./ar/home";
import { docsGuide as arDocsGuide } from "./ar/docs-guide";
import { chrome as ptBrChrome } from "./pt-BR/chrome";
import { home as ptBrHome } from "./pt-BR/home";
import { chrome as idChrome } from "./id/chrome";
import { home as idHome } from "./id/home";

const CHROME: Record<string, ChromeDict> = {
  zh: zhChrome,
  ja: jaChrome,
  vi: viChrome,
  ko: koChrome,
  ru: ruChrome,
  uk: ukChrome,
  es: esChrome,
  fr: frChrome,
  de: deChrome,
  ca: caChrome,
  hi: hiChrome,
  tr: trChrome,
  it: itChrome,
  pl: plChrome,
  ar: arChrome,
  "pt-BR": ptBrChrome,
  id: idChrome,
};

const HOME: Record<string, HomeDict> = {
  zh: zhHome,
  ja: jaHome,
  vi: viHome,
  ko: koHome,
  ru: ruHome,
  uk: ukHome,
  es: esHome,
  fr: frHome,
  de: deHome,
  ca: caHome,
  hi: hiHome,
  tr: trHome,
  it: itHome,
  pl: plHome,
  ar: arHome,
  "pt-BR": ptBrHome,
  id: idHome,
};

/** Locales with their own dictionary directory (English is the reference). */
export const DICTIONARY_LOCALES = Object.keys(CHROME) as readonly string[];

/**
 * Per-page dictionaries (#5337). Unlike chrome/home, a page dictionary is
 * optional per locale: English is the required reference, any locale that
 * ships the file is held to exact key parity, and everyone else falls back
 * to English here — the same behavior page bodies already had for partial
 * locales, now expressed through one lookup instead of an `isZh` ternary.
 */
const DOCS_GUIDE: Record<string, DocsGuideDict> = {
  zh: zhDocsGuide,
  fr: frDocsGuide,
  de: deDocsGuide,
  ca: caDocsGuide,
  hi: hiDocsGuide,
  tr: trDocsGuide,
  it: itDocsGuide,
  pl: plDocsGuide,
  ar: arDocsGuide,
};

const DOCS_SHELL: Record<string, DocsShellDict> = {
  zh: zhDocsShell,
};

const DOCS_HOOKS: Record<string, DocsHooksDict> = {
  zh: zhDocsHooks,
};

const DOCS_TROUBLESHOOTING: Record<string, DocsTroubleshootingDict> = {
  zh: zhDocsTroubleshooting,
};

const DOCS_CONFIGURATION: Record<string, DocsConfigurationDict> = {
  zh: zhDocsConfiguration,
};

const DOCS_FLEET: Record<string, DocsFleetDict> = {
  zh: zhDocsFleet,
};

const DOCS_MCP: Record<string, DocsMcpDict> = {
  zh: zhDocsMcp,
};

const DOCS_MODES: Record<string, DocsModesDict> = {
  zh: zhDocsModes,
};

const DOCS_RUNTIME_API: Record<string, DocsRuntimeApiDict> = {
  zh: zhDocsRuntimeApi,
};

const DOCS_SANDBOX: Record<string, DocsSandboxDict> = {
  zh: zhDocsSandbox,
};

const DOCS_SUBAGENTS: Record<string, DocsSubagentsDict> = {
  zh: zhDocsSubagents,
};

const DOCS_WEB: Record<string, DocsWebDict> = {
  zh: zhDocsWeb,
};

const DOCS_WORK: Record<string, DocsWorkDict> = {
  zh: zhDocsWork,
};

const DOCS_PET: Record<string, DocsPetDict> = {
  zh: zhDocsPet,
};

const DOCS_AUTH: Record<string, DocsAuthDict> = {
  zh: zhDocsAuth,
};

const COMPUTER_USE: Record<string, ComputerUseDict> = {
  zh: zhComputerUse,
  ja: jaComputerUse,
  vi: viComputerUse,
  ko: koComputerUse,
  ru: ruComputerUse,
  uk: ukComputerUse,
  es: esComputerUse,
  fr: frComputerUse,
  de: deComputerUse,
  ca: caComputerUse,
  hi: hiComputerUse,
  tr: trComputerUse,
  it: itComputerUse,
  pl: plComputerUse,
  ar: arComputerUse,
  "pt-BR": ptBrComputerUse,
  id: idComputerUse,
};

const DOCS_TRUST: Record<string, DocsTrustDict> = {
  zh: zhDocsTrust,
};

const DOCS_REVIEW: Record<string, DocsReviewDict> = {
  zh: zhDocsReview,
};

const DOCS_VOCABULARY: Record<string, DocsVocabularyDict> = {
  zh: zhDocsVocabulary,
};

/**
 * Shared surface states, the changelog page, the two legal pages, the digest,
 * feed and community pages, the FAQ, the roadmap, and the contribute,
 * constitution and runtime pages follow the same optional per-locale rule as
 * the docs page dictionaries: English is the reference, every other locale
 * falls back to it at lookup time.
 */
const STATES: Record<string, StatesDict> = {
  zh: zhStates,
};

const CHANGELOG: Record<string, ChangelogDict> = {
  zh: zhChangelog,
};

const LEGAL_TERMS: Record<string, LegalTermsDict> = {
  zh: zhLegalTerms,
};

const LEGAL_PRIVACY: Record<string, LegalPrivacyDict> = {
  zh: zhLegalPrivacy,
};

const DIGEST: Record<string, DigestDict> = {
  zh: zhDigest,
};

const FEED: Record<string, FeedDict> = {
  zh: zhFeed,
};

const COMMUNITY: Record<string, CommunityDict> = {
  zh: zhCommunity,
};

const FAQ: Record<string, FaqDict> = {
  zh: zhFaq,
};

const ROADMAP: Record<string, RoadmapDict> = {
  zh: zhRoadmap,
};

const CONTRIBUTE: Record<string, ContributeDict> = {
  zh: zhContribute,
};

const CONSTITUTION: Record<string, ConstitutionDict> = {
  zh: zhConstitution,
};

const RUNTIME: Record<string, RuntimeDict> = {
  zh: zhRuntime,
};

const FEATURES: Record<string, FeaturesDict> = {
  "en": features0,
  "zh": features1,
  "ja": features2,
  "vi": features3,
  "ko": features4,
  "ru": features5,
  "uk": features6,
  "es": features7,
  "pt-BR": features8,
  "fr": features9,
  "de": features10,
  "ca": features11,
  "id": features12,
  "hi": features13,
  "tr": features14,
  "it": features15,
  "pl": features16,
  "ar": features17,
};

export function getFeatures(locale: string): FeaturesDict {
  return FEATURES[locale] ?? features0;
}

export function getChrome(locale: string): ChromeDict {
  return CHROME[locale] ?? enChrome;
}

export function getHome(locale: string): HomeDict {
  return HOME[locale] ?? enHome;
}

export function getDocsGuide(locale: string): DocsGuideDict {
  return DOCS_GUIDE[locale] ?? enDocsGuide;
}

export function getDocsShell(locale: string): DocsShellDict {
  return DOCS_SHELL[locale] ?? enDocsShell;
}

export function getDocsHooks(locale: string): DocsHooksDict {
  return DOCS_HOOKS[locale] ?? enDocsHooks;
}

export function getDocsTroubleshooting(locale: string): DocsTroubleshootingDict {
  return DOCS_TROUBLESHOOTING[locale] ?? enDocsTroubleshooting;
}

export function getDocsConfiguration(locale: string): DocsConfigurationDict {
  return DOCS_CONFIGURATION[locale] ?? enDocsConfiguration;
}

export function getDocsFleet(locale: string): DocsFleetDict {
  return DOCS_FLEET[locale] ?? enDocsFleet;
}

export function getDocsMcp(locale: string): DocsMcpDict {
  return DOCS_MCP[locale] ?? enDocsMcp;
}

export function getDocsModes(locale: string): DocsModesDict {
  return DOCS_MODES[locale] ?? enDocsModes;
}

export function getDocsRuntimeApi(locale: string): DocsRuntimeApiDict {
  return DOCS_RUNTIME_API[locale] ?? enDocsRuntimeApi;
}

export function getDocsSandbox(locale: string): DocsSandboxDict {
  return DOCS_SANDBOX[locale] ?? enDocsSandbox;
}

export function getDocsSubagents(locale: string): DocsSubagentsDict {
  return DOCS_SUBAGENTS[locale] ?? enDocsSubagents;
}

export function getDocsWeb(locale: string): DocsWebDict {
  return DOCS_WEB[locale] ?? enDocsWeb;
}

export function getDocsWork(locale: string): DocsWorkDict {
  return DOCS_WORK[locale] ?? enDocsWork;
}

export function getDocsPet(locale: string): DocsPetDict {
  return DOCS_PET[locale] ?? enDocsPet;
}

export function getDocsAuth(locale: string): DocsAuthDict {
  return DOCS_AUTH[locale] ?? enDocsAuth;
}

export function getDocsTrust(locale: string): DocsTrustDict {
  return DOCS_TRUST[locale] ?? enDocsTrust;
}

export function getDocsReview(locale: string): DocsReviewDict {
  return DOCS_REVIEW[locale] ?? enDocsReview;
}

export function getDocsVocabulary(locale: string): DocsVocabularyDict {
  return DOCS_VOCABULARY[locale] ?? enDocsVocabulary;
}

export function getComputerUse(locale: string): ComputerUseDict {
  return COMPUTER_USE[locale] ?? enComputerUse;
}

/**
 * Locales whose /computer-use page body is translated (the page renders only
 * from this dictionary). The content-locale registry derives canonical,
 * hreflang and sitemap coverage from it, so they cannot drift apart.
 */
export function hasComputerUseTranslation(locale: string): boolean {
  return locale === "en" || Object.hasOwn(COMPUTER_USE, locale);
}

export function getStates(locale: string): StatesDict {
  return STATES[locale] ?? enStates;
}

export function getChangelog(locale: string): ChangelogDict {
  return CHANGELOG[locale] ?? enChangelog;
}

export function getLegalTerms(locale: string): LegalTermsDict {
  return LEGAL_TERMS[locale] ?? enLegalTerms;
}

export function getLegalPrivacy(locale: string): LegalPrivacyDict {
  return LEGAL_PRIVACY[locale] ?? enLegalPrivacy;
}

export function getDigest(locale: string): DigestDict {
  return DIGEST[locale] ?? enDigest;
}

export function getFeed(locale: string): FeedDict {
  return FEED[locale] ?? enFeed;
}

export function getCommunity(locale: string): CommunityDict {
  return COMMUNITY[locale] ?? enCommunity;
}

export function getFaq(locale: string): FaqDict {
  return FAQ[locale] ?? enFaq;
}

export function getRoadmap(locale: string): RoadmapDict {
  return ROADMAP[locale] ?? enRoadmap;
}

export function getContribute(locale: string): ContributeDict {
  return CONTRIBUTE[locale] ?? enContribute;
}

export function getConstitution(locale: string): ConstitutionDict {
  return CONSTITUTION[locale] ?? enConstitution;
}

export function getRuntime(locale: string): RuntimeDict {
  return RUNTIME[locale] ?? enRuntime;
}

/**
 * Select one side of a legacy `{ en, zh }` content pair by locale. This is
 * the transitional bridge for `web/lib/content/` modules that still carry
 * two-language pairs (#5337 Phase 3 dissolves them into dictionaries): it
 * moves the branch out of page TSX and into the i18n layer, so call sites
 * stay locale-agnostic.
 */
export function pickText(pair: { en: string; zh: string }, locale: string): string {
  return pair[pickTextLocale(locale)];
}

/** The actual language selected by the legacy two-language content bridge. */
export function pickTextLocale(locale: string): "en" | "zh" {
  return locale === "zh" ? "zh" : "en";
}

/** Reference dictionaries (parity baseline for the locale checks). */
export const EN_CHROME = enChrome;
export const EN_HOME = enHome;
export const EN_DOCS_GUIDE = enDocsGuide;
export const EN_DOCS_SHELL = enDocsShell;
export const EN_DOCS_HOOKS = enDocsHooks;
export const EN_DOCS_TROUBLESHOOTING = enDocsTroubleshooting;
export const EN_DOCS_CONFIGURATION = enDocsConfiguration;
export const EN_DOCS_FLEET = enDocsFleet;
export const EN_DOCS_MCP = enDocsMcp;
export const EN_DOCS_MODES = enDocsModes;
export const EN_DOCS_RUNTIME_API = enDocsRuntimeApi;
export const EN_DOCS_SANDBOX = enDocsSandbox;
export const EN_DOCS_SUBAGENTS = enDocsSubagents;
export const EN_DOCS_WEB = enDocsWeb;
export const EN_DOCS_WORK = enDocsWork;
export const EN_DOCS_PET = enDocsPet;
export const EN_DOCS_AUTH = enDocsAuth;
export const EN_DOCS_TRUST = enDocsTrust;
export const EN_DOCS_REVIEW = enDocsReview;
export const EN_DOCS_VOCABULARY = enDocsVocabulary;
export const EN_COMPUTER_USE = enComputerUse;
export const EN_STATES = enStates;
export const EN_CHANGELOG = enChangelog;
export const EN_LEGAL_TERMS = enLegalTerms;
export const EN_LEGAL_PRIVACY = enLegalPrivacy;
export const EN_DIGEST = enDigest;
export const EN_FEED = enFeed;
export const EN_COMMUNITY = enCommunity;
export const EN_FAQ = enFaq;
export const EN_ROADMAP = enRoadmap;
export const EN_CONTRIBUTE = enContribute;
export const EN_CONSTITUTION = enConstitution;
export const EN_RUNTIME = enRuntime;

/** Interpolate `{name}` tokens in a dictionary template. Unknown tokens are
 * left intact so a template/variable drift is visible in review, not silent. */
export function fill(template: string, vars: Record<string, string | number>): string {
  return template.replace(/\{(\w+)\}/g, (match, name: string) =>
    name in vars ? String(vars[name]) : match,
  );
}

/**
 * Split a template on a single `{token}` so a call site can typeset the
 * substituted value as its own element without concatenating translated
 * fragments around it. Returns the literal parts in template order — the
 * caller interleaves its node between them, so a locale that puts the token
 * in a different position still renders correctly.
 */
export function splitToken(template: string, token: string): string[] {
  return template.split(`{${token}}`);
}


export function getDocsSkills(locale: string): DocsPageDict {
  return locale === "zh" ? zhDocsSkills : enDocsSkills;
}

export function getEnterprise(locale: string): EnterpriseDict {
  return locale === "zh" ? zhEnterprise : enEnterprise;
}

export function getDocsClaudeCode(locale: string): DocsPageDict {
  return locale === "zh" ? zhDocsClaudeCode : enDocsClaudeCode;
}
