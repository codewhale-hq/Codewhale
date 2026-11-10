import { DISCORD_URL, REPO_URL } from "./i18n/links";
import { IDENTITY_PHRASE, SITE_NAME, SITE_URL } from "./page-meta";

export const ORGANIZATION_ID = `${SITE_URL}/#organization`;
export const WEBSITE_ID = `${SITE_URL}/#website`;
export const ORGANIZATION_LOGO_URL = `${SITE_URL}/icon-512.png`;

/**
 * Site-wide Organization + WebSite graph.
 * `inLanguage` is the routed locale of the document that embeds this block.
 */
export function buildSiteJsonLd(locale: string) {
  return {
    "@context": "https://schema.org",
    "@graph": [
      {
        "@type": "Organization",
        "@id": ORGANIZATION_ID,
        name: SITE_NAME,
        url: SITE_URL,
        logo: {
          "@type": "ImageObject",
          url: ORGANIZATION_LOGO_URL,
        },
        sameAs: [REPO_URL, DISCORD_URL],
      },
      {
        "@type": "WebSite",
        "@id": WEBSITE_ID,
        name: SITE_NAME,
        url: SITE_URL,
        description: IDENTITY_PHRASE,
        inLanguage: locale,
        publisher: { "@id": ORGANIZATION_ID },
      },
    ],
  };
}
