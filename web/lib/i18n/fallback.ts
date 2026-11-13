import { defaultLocale } from "./config";
import { contentLocalesForPath } from "./content-locales";
import { EN_CHROME, EN_DOCS_GUIDE, EN_DOCS_SHELL, getChrome, getDocsGuide, getDocsShell } from "./dictionaries";

export interface FallbackAttrs {
  lang: "en";
  dir: "ltr";
}

export const ENGLISH_ATTRS: FallbackAttrs = { lang: "en", dir: "ltr" };

export const DOCS_TOPICS = [
  "account",
  "auth",
  "claude-code",
  "computers",
  "configuration",
  "constitution",
  "first-time",
  "fleet",
  "guide",
  "hooks",
  "mcp",
  "modes",
  "pet",
  "plugin-authoring",
  "pod",
  "review",
  "runtime-api",
  "runtime-reference",
  "sandbox",
  "skills",
  "subagents",
  "tools",
  "troubleshooting",
  "trust",
  "vocabulary",
  "web",
  "work",
  "workflow-tutorial",
] as const;

function normalize(path: string): string {
  return path === "" || path === "/" ? "/" : `/${path.replace(/^\/+|\/+$/g, "")}`;
}

export function bodyFallsBackToEnglish(path: string, locale: string): boolean {
  if (locale === defaultLocale) return false;
  return !contentLocalesForPath(normalize(path)).includes(locale);
}

export function fallbackAttrs(path: string, locale: string): FallbackAttrs | undefined {
  return bodyFallsBackToEnglish(path, locale) ? ENGLISH_ATTRS : undefined;
}

export function bodyDateLocale(path: string, locale: string): string {
  return bodyFallsBackToEnglish(path, locale) ? EN_CHROME.dateLocale : getChrome(locale).dateLocale;
}

export function docsChromeFallsBackToEnglish(locale: string): boolean {
  return locale !== defaultLocale && getDocsShell(locale) === EN_DOCS_SHELL;
}

export function docsEnglishTopics(locale: string): string[] {
  if (locale === defaultLocale) return [];
  return ["", ...DOCS_TOPICS].filter((topic) => {
    if (topic === "guide" && getDocsGuide(locale) !== EN_DOCS_GUIDE) return false;
    return bodyFallsBackToEnglish(topic ? `/docs/${topic}` : "/docs", locale);
  });
}
