import Link from "next/link";
import { getFacts } from "@/lib/facts";
import { buildPageMetadata } from "@/lib/page-meta";
import { MODELS_COPY } from "@/lib/content/models";
import { Icon } from "@/components/icon";
import { ModelsTable } from "@/components/models-table";
import { PageHeader, Section } from "@/components/page-header";
import type { LocalizedText } from "@/lib/content/vocabulary";
import { fill, pickText } from "@/lib/i18n/dictionaries";

export const revalidate = 300;

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return buildPageMetadata({ path: "/models", locale,
    title: pickText(MODELS_COPY.metaTitle, locale),
    description: pickText(MODELS_COPY.metaDescription, locale) });
}

/**
 * /models — connect a provider, then the source's own model and provider
 * lists (facts layer), never a hand-typed catalogue.
 */
export default async function ModelsPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const facts = await getFacts();
  const t = (copy: LocalizedText) => fill(pickText(copy, locale), {
    version: facts.version ?? "—", model: facts.defaultModel ?? "—",
  });
  const providerDocs = "https://github.com/codewhale-hq/CodeWhale/blob/main/docs/PROVIDERS.md";
  const docsLink = (
    <Link href={providerDocs} className="section-link">
      {t(MODELS_COPY.providerDocs)}
      <Icon name="external" className="icon" />
    </Link>
  );

  return (
    <>
      <PageHeader
        title={t(MODELS_COPY.title)}
        lede={t(MODELS_COPY.lead)}
        pose="think"
        actions={
          <>
            <Link href={providerDocs} className="btn btn-primary btn-lg">{t(MODELS_COPY.providerDocs)}</Link>
            <Link href={`/${locale}/install`} className="btn btn-secondary btn-lg">{t(MODELS_COPY.install)}</Link>
          </>
        }
      />

      <div className="page-body">
        <Section
          id="models-setup"
          layout="split"
          title={t(MODELS_COPY.setupTitle)}
          scope={t(MODELS_COPY.setupLead)}
          link={docsLink}
        >
          <dl className="ruled-list">
            {MODELS_COPY.patterns.map((pattern) => (
              <div key={pattern.reference}>
                <dt>{pattern.reference}</dt>
                <dd>
                  <strong>{t(pattern.title)}</strong> · {t(pattern.detail)}
                </dd>
              </div>
            ))}
          </dl>
        </Section>

        <Section id="models-title" title={t(MODELS_COPY.modelsTitle)} scope={t(MODELS_COPY.modelsLead)} link={docsLink}>
          <ModelsTable models={facts.models} locale={locale} />
        </Section>

        <Section id="providers-title" title={t(MODELS_COPY.listTitle)} scope={t(MODELS_COPY.listLead)} link={docsLink}>
          <div className="data-table-wrap">
            <table className="data-table">
              <caption className="sr-only">{t(MODELS_COPY.listTitle)}</caption>
              <thead>
                <tr>
                  <th scope="col">{t(MODELS_COPY.provider)}</th>
                  <th scope="col">{t(MODELS_COPY.id)}</th>
                  <th scope="col">{t(MODELS_COPY.credential)}</th>
                </tr>
              </thead>
              <tbody>
                {facts.providers.map((provider) => (
                  <tr key={provider.id}>
                    <th scope="row">{provider.label}</th>
                    <td><code>{provider.id}</code></td>
                    <td><code>{provider.env}</code></td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <p className="section-scope mt-5">
            {t(MODELS_COPY.missing)}{" "}
            <Link href="https://github.com/codewhale-hq/CodeWhale/issues/new/choose" className="link">{t(MODELS_COPY.request)}</Link>
          </p>
        </Section>
      </div>
    </>
  );
}
