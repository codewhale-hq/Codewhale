import Link from "next/link";
import type { Locale } from "@/lib/i18n/config";
import { getChrome } from "@/lib/i18n/dictionaries";
import { navLinks, secondaryNavLinks, REPO_URL } from "@/lib/i18n/links";
import { Icon } from "./icon";
import { LocaleSwitcher } from "./locale-switcher";
import { MobileMenu } from "./mobile-menu";
import { NavLinks } from "./nav-links";
import { ThemeToggle } from "./theme-toggle";
import { WhalePose } from "./whale-pose";
import { ArrowLabel } from "./arrow-label";

/**
 * Masthead + primary nav. Like the GPUI titlebar it carries few visible
 * controls: the whale and the wordmark, five links, quiet icon controls
 * with accessible names and the one primary action, Install.
 */
export function Nav({ locale = "en" }: { locale?: Locale }) {
  const chrome = getChrome(locale);
  const links = navLinks(locale, chrome);
  const moreLinks = secondaryNavLinks(locale, chrome);
  const homeHref = `/${locale}`;

  return (
    <header className="site-nav paper-nav">
      <div className="site-nav-inner paper-nav-inner">
        <Link href={homeHref} className="site-wordmark paper-wordmark" aria-label={chrome.navHomeAria}>
          <div className="paper-wordmark-text">
            <WhalePose pose="rest" className="paper-wordmark-mark" priority />
            {/* The wordmark is one ink, drawn through a CSS mask so both
                appearances re-ink it; the link carries the name. */}
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
          <Link href={REPO_URL} className="nav-icon-button site-github-link" aria-label="GitHub" title="GitHub">
            <Icon name="github" className="nav-icon" />
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
            moreLinks={moreLinks}
            openLabel={chrome.menuOpen}
            closeLabel={chrome.menuClose}
            navAria={chrome.navPrimaryAria}
          />
        </div>
      </div>
    </header>
  );
}
