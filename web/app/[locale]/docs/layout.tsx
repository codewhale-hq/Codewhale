import { DocsArticleSearch } from "@/components/docs-search";
import { DocsBreadcrumb } from "@/components/docs-breadcrumb";
import { DocsHelp } from "@/components/docs-help";
import { DocsSidebar } from "@/components/docs-sidebar";
import { ReleaseTruth } from "@/components/release-truth";
import { WhalePose } from "@/components/whale-pose";
import { BUILD_FACTS, getFactsWithProvenance } from "@/lib/facts";
import { getDocsShell } from "@/lib/i18n/dictionaries";

/* ------------------------------------------------------------------ */
/*  Layout (Next.js App Router)                                        */
/* ------------------------------------------------------------------ */

/**
 * The docs shell every documentation URL shares: the portal hero with the
 * version-aware release-truth line, the breadcrumb + content + sidebar
 * grid, and the contextual help band that closes each page. All copy is
 * dictionary-driven; the release line reads the facts layer.
 */
export default async function DocsLayout({
  children,
  params,
}: {
  children: React.ReactNode;
  params: Promise<{ locale: string }>;
}) {
  const { locale } = await params;
  const t = getDocsShell(locale);
  // These pages describe THIS build: pin the documented facts to the build
  // snapshot even when the KV snapshot was written by a newer source (a
  // rollback, or any deployment sharing it). Only the latest published
  // release is the KV snapshot's to speak for.
  const resolution = await getFactsWithProvenance();
  const facts = {
    ...BUILD_FACTS,
    latestPublishedRelease: resolution.facts.latestPublishedRelease,
  };

  return (
    <div className="docs-theme docs-portal min-h-screen">
      <section className="hero">
        <div className="portal-container docs-portal-band">
          <div className="portal-mark">
            <WhalePose pose="read" className="portal-mark-pose" priority />
            <span>{t.portalMark}</span>
          </div>
          <ReleaseTruth locale={locale} facts={facts} />
        </div>
      </section>

      <div className="portal-container docs-shell min-w-0">
        <article className="docs-content min-w-0">
          <DocsBreadcrumb locale={locale} />
          <DocsArticleSearch locale={locale} />
          {children}
          <DocsHelp locale={locale} />
        </article>
        <DocsSidebar locale={locale} />
      </div>
    </div>
  );
}
