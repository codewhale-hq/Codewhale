import type { PublishedReleaseFact } from "./facts";
import { releasedFeatureNames } from "./capabilities";
import { REPO_URL } from "./i18n/links";
import { desktopDownload } from "./desktop-release";
import { getHome } from "./i18n/dictionaries";
import { SITE_NAME, SITE_URL } from "./page-meta";
import { ORGANIZATION_ID } from "./site-schema";

/**
 * Structured product data for the release-backed install page.
 *
 * Source candidates deliberately do not enter this function: the download URL
 * resolves published artifacts, so softwareVersion must describe that release
 * or be absent when no published release receipt is available.
 */
export function buildSoftwareApplicationJsonLd(
  publishedRelease: (Pick<PublishedReleaseFact, "version"> & Partial<Pick<PublishedReleaseFact, "publishedAt" | "url">>) | null,
  locale = "en",
) {
  const installUrl = `${SITE_URL}/${locale}/install`;
  return {
    "@context": "https://schema.org",
    "@type": "SoftwareApplication",
    "@id": `${SITE_URL}/#software`,
    name: SITE_NAME,
    url: SITE_URL,
    description: getHome(locale).metaDescription,
    applicationCategory: "DeveloperApplication",
    operatingSystem: "macOS, Linux, Windows",
    ...(publishedRelease?.version ? { softwareVersion: publishedRelease.version } : {}),
    ...(publishedRelease?.version ? { featureList: releasedFeatureNames(publishedRelease.version) } : {}),
    ...(publishedRelease?.version && publishedRelease.url ? { releaseNotes: publishedRelease.url } : {}),
    ...(publishedRelease?.version && publishedRelease.publishedAt ? { dateModified: publishedRelease.publishedAt } : {}),
    installUrl,
    sameAs: [REPO_URL, "https://www.npmjs.com/package/codewhale", "https://crates.io/crates/codewhale-cli"],
    license: `${REPO_URL}/blob/main/LICENSE`,
    codeRepository: REPO_URL,
    downloadUrl: desktopDownload()?.url ?? installUrl,
    author: { "@id": ORGANIZATION_ID },
  };
}
