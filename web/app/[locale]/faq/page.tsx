import { faqEn, faqZh } from "@/lib/content/faq";
import { PageHeader } from "@/components/page-header";
import { FaqSearch } from "@/components/faq-search";
import { buildFaqPageJsonLd } from "@/lib/faq-schema";
import { canonicalLocaleForPath } from "@/lib/i18n/content-locales";
import { getFaq, pickTextLocale } from "@/lib/i18n/dictionaries";
import { serializeJsonLd } from "@/lib/json-ld";
import { buildPageMetadata } from "@/lib/page-meta";
import { SITE_URL } from "@/lib/page-meta";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getFaq(locale);
  return buildPageMetadata({
    path: "/faq",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function FaqPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getFaq(locale);
  const p = (path: string) => `/${locale}${path}`;
  const items = { en: faqEn, zh: faqZh }[pickTextLocale(locale)](p);
  const canonicalLocale = canonicalLocaleForPath("/faq", locale);
  const jsonLd = buildFaqPageJsonLd({
    items,
    url: `${SITE_URL}/${canonicalLocale}/faq`,
    inLanguage: canonicalLocale,
  });

  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: serializeJsonLd(jsonLd) }}
      />
      <PageHeader
        seal="问"
        kicker={t.eyebrow}
        title={t.title}
        titleAside={t.titleAside}
        titleAsideLang={t.titleAsideLang}
        lede={t.lead}
        pose="talk"
      />

      <div className="page-body">
        <div className="page-body-narrow">
          <FaqSearch items={items} locale={locale} />

          <div className="empty-state empty-state-compact faq-more">
            <p className="empty-state-title">{t.notCovered}</p>
            <div className="empty-state-actions">
              <a href="https://github.com/codewhale-hq/CodeWhale/issues/new/choose" className="btn btn-secondary">
                {t.openIssue}
              </a>
            </div>
          </div>
        </div>
      </div>
    </>
  );
}
