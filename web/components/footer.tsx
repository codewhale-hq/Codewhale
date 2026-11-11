import Link from "next/link";
import type { Locale } from "@/lib/i18n/config";
import { getChrome } from "@/lib/i18n/dictionaries";
import {
  footerDocsLinks,
  footerLegalLinks,
  footerProductLinks,
  footerProjectLinks,
  type ChromeLink,
} from "@/lib/i18n/links";
import { SITE_CONTACT_EMAIL } from "@/lib/page-meta";
import { reviewState } from "@/lib/i18n/review-state";
import { USAGE_COUNTING_COPY } from "@/lib/content/usage-counting";
import { pickText } from "@/lib/i18n/dictionaries";

function FooterList({ links }: { links: ChromeLink[] }) {
  return (
    <div className="site-footer-list">
      {links.map((item) => <Link key={item.href} href={item.href}>{item.label}</Link>)}
    </div>
  );
}

/**
 * Site footer: the page's own ground under one hairline, in both
 * appearances. One dictionary path for every routed locale; four short link
 * columns (Product, Docs, Project, Legal), which fold into four disclosures
 * on a phone.
 */
export function Footer({ locale = "en" }: { locale?: Locale }) {
  const chrome = getChrome(locale);
  const homeHref = `/${locale}`;
  const groups: { label: string; links: ChromeLink[] }[] = [
    { label: chrome.footerProduct, links: footerProductLinks(locale, chrome) },
    { label: chrome.footerDocs, links: footerDocsLinks(locale, chrome) },
    { label: chrome.footerProject, links: footerProjectLinks(locale, chrome) },
    { label: chrome.footerLegal, links: footerLegalLinks(locale, chrome, pickText(USAGE_COUNTING_COPY.footerLink, locale)) },
  ];

  return (
    <footer className="site-footer">
      <div className="site-footer-main">
        <div className="site-footer-brand">
          <Link href={homeHref} className="site-wordmark site-wordmark-footer" aria-label="Codewhale">
            <span className="brand-mark" aria-hidden="true" />
            <span className="wordmark" aria-hidden="true" />
          </Link>
        </div>

        <div className="site-footer-links">
          {groups.map((group) => (
            <div key={group.label} className="site-footer-group">
              <span className="site-footer-label">{group.label}</span>
              <FooterList links={group.links} />
            </div>
          ))}
        </div>
        <div className="site-footer-folds">
          {groups.map((group) => (
            <details key={group.label} className="site-footer-fold" name="site-footer">
              <summary>{group.label}</summary>
              <FooterList links={group.links} />
            </details>
          ))}
        </div>
      </div>

      <div className="site-footer-meta">
        <a href={`mailto:${SITE_CONTACT_EMAIL}`}>{SITE_CONTACT_EMAIL}</a>
        <span>© {new Date().getFullYear()} Codewhale</span>
        {reviewState(locale) === "machine" && <span className="site-footer-review">{chrome.translationMachineDraft}</span>}
      </div>
    </footer>
  );
}
