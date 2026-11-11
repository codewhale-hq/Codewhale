import Link from "next/link";
import { LegalTabs } from "@/components/legal-tabs";
import { PageHeader } from "@/components/page-header";
import { getLegalTerms } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";
import { formatLegalDocumentStatus, LEGAL_DOCUMENTS, TermsContent } from "@/lib/legal-copy";
import { bodyDateLocale } from "@/lib/i18n/fallback";
import { FallbackRegion } from "@/components/fallback-region";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getLegalTerms(locale);
  return buildPageMetadata({
    path: "/legal/terms",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function TermsPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getLegalTerms(locale);
  return (
    <FallbackRegion path="/legal/terms" locale={locale}>
      <PageHeader
        kicker={t.kicker}
        title={t.title}
        meta={formatLegalDocumentStatus("terms", bodyDateLocale("/legal/terms", locale))}
      />
      <div className="page-body">
        <div className="page-body-narrow">
          <LegalTabs locale={locale} current="terms" />
          <article className="prose legal-doc [&>section>p+p]:mt-[var(--space-4)]" data-legal-version={LEGAL_DOCUMENTS.terms.version} data-legal-status={LEGAL_DOCUMENTS.terms.status} data-legal-effective-at={LEGAL_DOCUMENTS.terms.effectiveAt ?? undefined}>
            <TermsContent />
            <p className="status-line">
              <Link href={`/${locale}/legal/privacy`} className="link">{t.privacyLink}</Link>
              <Link href={`/${locale}`} className="link">{t.homeLink}</Link>
            </p>
          </article>
        </div>
      </div>
    </FallbackRegion>
  );
}
