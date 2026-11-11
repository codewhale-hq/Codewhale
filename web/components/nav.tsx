import Link from "next/link";
import type { Locale } from "@/lib/i18n/config";
import { getChrome } from "@/lib/i18n/dictionaries";
import { navLinks, signInLink, REPO_URL } from "@/lib/i18n/links";
import { formatStars, readRepoStatsSnapshot } from "@/lib/github";
import { PROJECT_COPY } from "@/lib/content/project";
import { fill, pickText } from "@/lib/i18n/dictionaries";
import { surfaceState } from "@/lib/surface-states";
import { Icon } from "./icon";
import { LocaleSwitcher } from "./locale-switcher";
import { MobileMenu } from "./mobile-menu";
import { NavLinks } from "./nav-links";
import { ThemeToggle } from "./theme-toggle";
import { ArrowLabel } from "./arrow-label";

/**
 * Masthead + primary nav. Like the GPUI titlebar it carries few visible
 * controls: the mark and the wordmark, the page links, quiet icon controls
 * with accessible names and the one primary action, Install.
 */
export function Nav({ locale = "en" }: { locale?: Locale }) {
  const chrome = getChrome(locale);
  const links = navLinks(locale, chrome, surfaceState("desktop") === "released");
  const signIn = signInLink(locale, chrome);
  const stats = readRepoStatsSnapshot();
  const stars = stats.stars === null ? null : formatStars(stats.stars);
  const asOf = stats.fetchedAt
    ? fill(chrome.numbersAsOf, {
        date: new Intl.DateTimeFormat(chrome.dateLocale, { dateStyle: "medium", timeZone: "UTC" }).format(new Date(stats.fetchedAt)),
      })
    : undefined;
  const githubAria = stars === null
    ? "GitHub"
    : [`GitHub, ${stars} ${pickText(PROJECT_COPY.statStars, locale)}`, asOf].filter(Boolean).join(", ");
  const homeHref = `/${locale}`;

  return (
    <header className="site-nav paper-nav">
      <div className="site-nav-inner paper-nav-inner">
        <Link href={homeHref} className="site-wordmark paper-wordmark" aria-label={chrome.navHomeAria}>
          <div className="paper-wordmark-text">
            <span className="brand-mark" aria-hidden="true" />
            {/* The mark and the wordmark are one ink, drawn through CSS masks so
                both appearances re-ink them; the link carries the name. */}
            <span className="paper-wordmark-logo" aria-hidden="true" />
          </div>
        </Link>

        <NavLinks links={links} primaryAria={chrome.navPrimaryAria} />

        <div className="site-nav-actions">
          <ThemeToggle
            key={locale}
            lightLabel={chrome.themeLight}
            darkLabel={chrome.themeDark}
            ariaTemplate={chrome.themeAria}
          />
          <LocaleSwitcher current={locale} />
          <Link
            href={REPO_URL}
            className="nav-icon-button site-github-link !w-auto gap-1.5 px-2.5"
            aria-label={githubAria}
            title={asOf ?? "GitHub"}
            data-funnel="star_exit"
          >
            <Icon name="github" className="nav-icon" />
            {stars !== null && <span className="tabular text-sm">{stars}</span>}
          </Link>
          <Link href={signIn.href} className="nav-link site-signin-link hidden lg:inline-flex px-2">
            {signIn.label}
          </Link>
          <Link
            href={`/${locale}/install`}
            className="paper-install-cta hidden lg:inline-flex"
          >
            <ArrowLabel text={chrome.installCta} />
          </Link>
          <MobileMenu
            installHref={`/${locale}/install`}
            installLabel={chrome.installCta}
            links={links}
            github={{ href: REPO_URL, label: "GitHub", count: stars, aria: githubAria }}
            signIn={signIn}
            openLabel={chrome.menuOpen}
            closeLabel={chrome.menuClose}
            navAria={chrome.navPrimaryAria}
          />
        </div>
      </div>
    </header>
  );
}
