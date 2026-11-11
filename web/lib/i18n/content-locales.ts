import { defaultLocale, locales } from "./config";
import { hasComputerUseTranslation } from "./dictionaries";

/**
 * Routes whose page-body translation coverage differs from English/Chinese.
 *
 * `/docs/guide` is not listed: its French/German/Catalan/Hindi/Turkish/
 * Italian/Polish/Arabic dictionaries translate only the overview and headings,
 * while the getting-started steps that make up the body ship in English and
 * Chinese, so those variants canonicalize to English like any other partial
 * page.
 */
const ROUTE_CONTENT_LOCALES: Readonly<Record<string, readonly string[]>> = {
  "/": locales,
  "/install": ["en"],
  "/features": ["en"],
  "/docs/runtime-reference": ["en"],
  "/docs/workflow-tutorial": ["en"],
  "/docs/plugin-authoring": ["en"],
  "/computer-use": locales.filter(hasComputerUseTranslation),
};

/** Most first-party page bodies currently ship in English and Chinese. */
const DEFAULT_CONTENT_LOCALES = ["en", "zh"] as const;

function normalizedPath(path: string): string {
  if (path === "" || path === "/") return "/";
  return `/${path.replace(/^\/+|\/+$/g, "")}`;
}

/** Locales with a genuine page-body translation for an indexable route. */
export function contentLocalesForPath(path: string): readonly string[] {
  const normalized = normalizedPath(path);
  if (normalized.startsWith("/ratatui/")) return ["en"];
  return ROUTE_CONTENT_LOCALES[normalized] ?? DEFAULT_CONTENT_LOCALES;
}

/**
 * Canonical locale for a rendered route.
 *
 * Partial locale routes stay accessible in the product, but an English-body
 * fallback points crawlers at the English source instead of presenting a
 * duplicate as a translated page.
 */
export function canonicalLocaleForPath(path: string, locale: string): string {
  return contentLocalesForPath(path).includes(locale) ? locale : defaultLocale;
}
