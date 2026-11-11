import Link from "next/link";
import { getTopic } from "@/lib/docs-map";
import { renderInline } from "../_components/doc-article";
import { StatusBadge } from "@/components/status-badge";
import {
  ADVISORY_ROLE,
  MEASUREMENT_PRINCIPLES,
  PRODUCT_TERMS,
  ROUTE_IDENTITY,
  controlTermsFor,
} from "@/lib/content/vocabulary";
import { docsCapabilityContext } from "@/lib/docs-release";
import { getChrome, getDocsShell, getDocsVocabulary, pickText } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getDocsVocabulary(locale);
  return buildPageMetadata({
    path: "/docs/vocabulary",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function VocabularyPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getDocsVocabulary(locale);
  const { body: controlRows, source: inSourceRows } = controlTermsFor(await docsCapabilityContext());

  return (
    <section className="space-y-10">
      <section id="overview" className="scroll-mt-32">
        <h1 className="font-display text-3xl mb-1">{t.title}</h1>
        <p className={`${t.bodyClassName} mt-3`}>
          {t.lead}
        </p>
      </section>

      <section id="execution-nouns" className="scroll-mt-32">
        <h2 className="font-display text-2xl mb-1">
          {t.executionHeading}
        </h2>
        <div className="hairline-t mt-4">
          {PRODUCT_TERMS.map((row) => (
            <section key={row.term} className="py-4 hairline-b">
              <h3 className="font-display text-xl">
                {row.term}{" "}
                <span className="text-sm text-ink-mute font-normal">
                  = {pickText(row.short, locale)}
                </span>
              </h3>
              <p className={`${t.bodyClassName} mt-1 text-sm`}>{pickText(row.long, locale)}</p>
            </section>
          ))}
        </div>
      </section>

      <section id="control-nouns" className="scroll-mt-32">
        <h2 className="font-display text-2xl mb-1">
          {t.controlHeading}
        </h2>
        <p className={`${t.bodyClassName} mt-3`}>
          {t.controlLead}
        </p>
        <Link href={`/${locale}/docs/modes`} className="body-link">
          {pickText(getTopic("modes")!.label, locale)}
        </Link>
        <div className="hairline-t mt-4">
          {controlRows.map((row) => (
            <section key={row.term} className="py-4 hairline-b">
              <h3 className="font-display text-xl">{row.term}</h3>
              <p className={`${t.bodyClassName} mt-1 text-sm`}>
                {pickText(row.description, locale)}
              </p>
            </section>
          ))}
        </div>
      </section>

      {inSourceRows.length > 0 && (
        <section id="in-source" className="scroll-mt-32" data-release-scope="source">
          <h2 className="font-display text-2xl mb-1">{getChrome(locale).stateInSource}</h2>
          <p className={`${t.bodyClassName} mt-3`}>{renderInline(getDocsShell(locale).sourceLead, locale)}</p>
          <h3 className="font-display text-xl mt-6">{t.sourceControlHeading}</h3>
          <div className="hairline-t mt-4">
            {inSourceRows.map((row) => (
              <section key={row.term} className="py-4 hairline-b">
                <h3 className="font-display text-xl">{row.term}</h3>
                <p className={`${t.bodyClassName} mt-1 text-sm`}>
                  {pickText(row.description, locale)}
                </p>
              </section>
            ))}
          </div>
        </section>
      )}

      <section id="route-identity" className="scroll-mt-32">
        <h2 className="font-display text-2xl mb-1">
          {t.routeHeading}
        </h2>
        <div className="hairline-t mt-4">
          {ROUTE_IDENTITY.map((row) => (
            <section key={row.term} className="py-4 hairline-b">
              <h3 className="font-display text-xl">{row.term}</h3>
              <p className={`${t.bodyClassName} mt-1 text-sm`}>
                {pickText(row.description, locale)}
              </p>
            </section>
          ))}
        </div>
      </section>

      <section id="advisory-role" className="scroll-mt-32">
        <h2 className="font-display text-2xl mb-1">
          {t.advisoryHeading}
        </h2>
        <div className="hairline-t mt-4 py-4 hairline-b">
          <h3 className="font-display text-xl">{ADVISORY_ROLE.term}</h3>
          <p className={`${t.bodyClassName} mt-1 text-sm`}>
            {pickText(ADVISORY_ROLE.description, locale)}
          </p>
        </div>
      </section>

      <section id="measurement" className="scroll-mt-32">
        <h2 className="font-display text-2xl mb-1">
          {t.measurementHeading}
        </h2>
        <ul className="mt-4 space-y-3">
          {MEASUREMENT_PRINCIPLES.map((principle) => (
            <li key={principle.en} className={`${t.bodyClassName} text-sm`}>
              {pickText(principle, locale)}
            </li>
          ))}
        </ul>
        <p className={`${t.bodyClassName} mt-4 text-sm`}>
          <StatusBadge kind="unavailable" locale={locale} />{" "}
          {t.leaderboardNote}
        </p>
      </section>

      {/* Maintainer pointer: kept out of the rendered copy (experience mark 5). */}
      <div hidden data-source-note={t.sourceNote} />
    </section>
  );
}
