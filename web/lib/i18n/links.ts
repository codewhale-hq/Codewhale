/**
 * Locale-aware chrome link sets (#4934).
 *
 * Nav and footer used to carry hardcoded `/en/...` and `/zh/...` arrays plus
 * a thin dictionary branch for everything else, so foreign locales silently
 * lost the Start and FAQ links. There is now one generator per link set:
 * labels come from `ChromeDict`, hrefs come from the locale, and every
 * routed locale gets the identical route shape. Locale-swap parity is a
 * unit test (`docs-ia.test.ts`) instead of a string scrape over the TSX.
 */
import type { ChromeDict } from "./dictionaries/types";

export const REPO_URL = "https://github.com/codewhale-hq/CodeWhale";
export const REPO_ISSUES_URL = `${REPO_URL}/issues`;
export const REPO_RELEASES_URL = `${REPO_URL}/releases`;
export const REPO_LICENSE_URL = `${REPO_URL}/blob/main/LICENSE`;
/** Community chat. A brand name, so it is not a dictionary string. */
export const DISCORD_URL = "https://discord.gg/37gfS3ksug";

/** The hosted Codewhale app (account sign-in / sign-up). */
export const APP_URL = "https://app.codewhale.net";
export const APP_LOGIN_URL = `${APP_URL}/login`;
export const APP_SIGNUP_URL = `${APP_URL}/signup`;

/** A chrome link: one label per destination. */
export interface ChromeLink {
  href: string;
  label: string;
}

export const SECURITY_POLICY_URL = `${REPO_URL}/blob/main/SECURITY.md`;

export function navLinks(locale: string, chrome: ChromeDict, desktopReleased = false): ChromeLink[] {
  return [
    { href: `/${locale}/docs`, label: chrome.navDocs },
    { href: `/${locale}/models`, label: chrome.navModels },
    { href: `/${locale}/changelog`, label: chrome.footerChangelog },
    { href: `/${locale}/contribute`, label: chrome.navContribute },
    ...(desktopReleased ? [{ href: `/${locale}/app`, label: chrome.navDesktop }] : []),
  ];
}

export function signInLink(locale: string, chrome: ChromeDict): ChromeLink {
  return { href: `/${locale}/signin`, label: chrome.navSignIn };
}

/** Footer "Product" column: what it is and where to get it. */
export function footerProductLinks(locale: string, chrome: ChromeDict): ChromeLink[] {
  return [
    { href: `/${locale}/install`, label: chrome.footerInstall },
    { href: `/${locale}/models`, label: chrome.footerModels },
    { href: `/${locale}/plugins`, label: chrome.navPlugins },
    { href: `/${locale}/computer-use`, label: "Computer Use" },
  ];
}

/** Footer "Docs" column: reading material for people building on Codewhale. */
export function footerDocsLinks(locale: string, chrome: ChromeDict): ChromeLink[] {
  return [
    { href: `/${locale}/docs/guide`, label: chrome.footerGuide },
    { href: `/${locale}/docs`, label: chrome.footerDocs },
    { href: `/${locale}/changelog`, label: chrome.footerChangelog },
    { href: `/${locale}/faq`, label: chrome.footerFaq },
    { href: `/${locale}/ratatui`, label: "Ratatui" },
  ];
}

export function footerProjectLinks(locale: string, chrome: ChromeDict): ChromeLink[] {
  return [
    { href: REPO_URL, label: "GitHub" },
    { href: `/${locale}/contribute`, label: chrome.footerContribute },
    { href: DISCORD_URL, label: "Discord" },
    { href: `/${locale}/merch`, label: chrome.footerMerch },
    { href: REPO_LICENSE_URL, label: chrome.footerLicense },
  ];
}

/** Footer legal destinations — reachable, never a 404. */
export function footerLegalLinks(locale: string, chrome: ChromeDict, usageData: string): ChromeLink[] {
  return [
    { href: `/${locale}/legal/terms`, label: chrome.footerTerms },
    { href: `/${locale}/legal/privacy`, label: chrome.footerPrivacy },
    { href: `/${locale}/legal/privacy#usage-counting`, label: usageData },
    { href: SECURITY_POLICY_URL, label: chrome.footerSecurity },
  ];
}

/**
 * The one link in a chrome set that names the page currently being viewed.
 *
 * Nav and the compact sheet used to decide this per link with a plain
 * prefix test (`pathname === href || pathname.startsWith(href + "/")`).
 * Two nav links are ancestor and descendant of each other — `/de/docs` and
 * `/de/docs/guide` — so on the guide route that test was true for both:
 * the nav underline landed under Docs and Start at once, and two links
 * carried `aria-current="page"`, telling assistive technology the reader is
 * on two pages simultaneously.
 *
 * The longest matching href is the page; anything shorter is an ancestor
 * section, not a location. `/de/docs/configuration` is not a nav link, so
 * Docs still wins there — only the collision resolves.
 */
export function currentNavHref(
  links: readonly ChromeLink[],
  pathname: string,
): string | null {
  let current: string | null = null;
  for (const link of links) {
    const matches = pathname === link.href || pathname.startsWith(`${link.href}/`);
    if (!matches) continue;
    if (current === null || link.href.length > current.length) current = link.href;
  }
  return current;
}
