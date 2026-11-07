import Link from "next/link";
import { GITEE_ENABLED, type Locale } from "@/lib/i18n/config";
import { getChrome } from "@/lib/i18n/dictionaries";
import {
  footerLegalLinks,
  footerProductLinks,
  footerProjectLinks,
} from "@/lib/i18n/links";
import { SITE_CONTACT_EMAIL, SITE_SECURITY_EMAIL } from "@/lib/page-meta";
import { USAGE_COUNTING_COPY } from "@/lib/content/usage-counting";
import { pickText } from "@/lib/i18n/dictionaries";
import { getStorefrontCopy } from "@/lib/content/merch-storefront";
import { WhalePose } from "./whale-pose";

/**
 * Site footer: the sea below one horizon line, in both appearances.
 * One dictionary path for every routed locale; the Product column carries
 * the full link set everywhere.
 */
export function Footer({ locale = "en" }: { locale?: Locale }) {
  const chrome = getChrome(locale);
  const homeHref = `/${locale}`;
  const product = footerProductLinks(locale, chrome);
  const project = footerProjectLinks(locale, chrome);
  const legal = footerLegalLinks(locale, chrome);

  return (
    <footer className="site-footer">
      <div className="site-footer-sea">
        <div className="horizon" aria-hidden="true" />
        <div className="sea-texture" aria-hidden="true" />
        <div className="site-footer-main">
          <div className="site-footer-brand">
            <Link href={homeHref} className="site-wordmark site-wordmark-footer" aria-label="Codewhale">
              <WhalePose pose="rest" />
              <span className="wordmark" aria-hidden="true" />
            </Link>
            <p>{chrome.footerTagline}</p>
          </div>

          <div className="site-footer-links">
            <div className="site-footer-group">
              <span className="site-footer-label">{chrome.footerProduct}</span>
              <div className="site-footer-list">
                {product.map((item) => <Link key={item.href} href={item.href}>{item.label}</Link>)}
              </div>
            </div>
            <div className="site-footer-group">
              <span className="site-footer-label">{chrome.footerProject}</span>
              <div className="site-footer-list">
                {project.map((item) => <Link key={item.href} href={item.href}>{item.label}</Link>)}
                <Link href={`/${locale}/merch`}>{getStorefrontCopy(locale).merch}</Link>
              </div>
            </div>
          </div>
        </div>

        <div className="site-footer-meta">
          <div className="site-footer-meta-links">
            {legal.map((item) => <Link key={item.href} href={item.href}>{item.label}</Link>)}
            {GITEE_ENABLED && <a href="https://gitee.com/codewhale-hq/CodeWhale">Gitee</a>}
            <a href="https://cnb.cool/codewhale.net/codewhale">CNB</a>
            <a href="https://npmmirror.com/package/codewhale">npmmirror</a>
            <a href={`mailto:${SITE_CONTACT_EMAIL}`}>{SITE_CONTACT_EMAIL}</a>
            <a href={`mailto:${SITE_SECURITY_EMAIL}`}>{chrome.footerSecurity}</a>
            <Link href={`/${locale}/legal/privacy#usage-counting`}>{pickText(USAGE_COUNTING_COPY.footerLink, locale)}</Link>
          </div>
          <span>© {new Date().getFullYear()} Codewhale</span>
        </div>
      </div>
    </footer>
  );
}
