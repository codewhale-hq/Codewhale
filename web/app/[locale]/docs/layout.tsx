import { DocsArticleSearch } from "@/components/docs-search";
import { EnglishRegion } from "@/components/fallback-region";
import { DocsBreadcrumb } from "@/components/docs-breadcrumb";
import { DocsHelp } from "@/components/docs-help";
import { DocsLanguageRegion } from "@/components/docs-language-region";
import { DocsSidebar } from "@/components/docs-sidebar";
import { ReleaseTruth } from "@/components/release-truth";
import { BUILD_FACTS, getFactsWithProvenance } from "@/lib/facts";
import { docsChromeFallsBackToEnglish, docsEnglishTopics } from "@/lib/i18n/fallback";

/* ------------------------------------------------------------------ */
/*  Layout (Next.js App Router)                                        */
/* ------------------------------------------------------------------ */

/**
 * The docs shell every documentation URL shares: the breadcrumb, the
 * version-aware release-truth line, the content + sidebar grid, and the contextual help band that closes each page. All copy is
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
  // These pages describe THIS build: pin the documented facts to the build
  // snapshot even when the KV snapshot was written by a newer source (a
  // rollback, or any deployment sharing it). Only the latest published
  // release is the KV snapshot's to speak for.
  const resolution = await getFactsWithProvenance();
  const facts = {
    ...BUILD_FACTS,
    latestPublishedRelease: resolution.facts.latestPublishedRelease,
  };

  const chromeEnglish = docsChromeFallsBackToEnglish(locale);

  return (
    <div className="docs-theme docs-portal min-h-screen">
      <div className="portal-container docs-shell min-w-0">
        <article className="docs-content min-w-0">
          <EnglishRegion when={chromeEnglish}>
            <DocsBreadcrumb locale={locale} />
            <ReleaseTruth locale={locale} facts={facts} />
            <DocsArticleSearch locale={locale} />
          </EnglishRegion>
          <DocsLanguageRegion locale={locale} englishTopics={docsEnglishTopics(locale)}>
            {children}
          </DocsLanguageRegion>
          <EnglishRegion when={chromeEnglish}>
            <DocsHelp locale={locale} />
          </EnglishRegion>
        </article>
        <EnglishRegion when={chromeEnglish}>
          <DocsSidebar locale={locale} />
        </EnglishRegion>
      </div>
    </div>
  );
}
