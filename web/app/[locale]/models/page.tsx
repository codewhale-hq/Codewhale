import Link from "next/link";
import { releaseView } from "@/lib/capabilities";
import { getFacts } from "@/lib/facts";
import { buildPageMetadata } from "@/lib/page-meta";
import {
  MODELS_COPY,
  ROUTE_PROVIDERS,
  formatModelDate,
  formatModelTokens,
  latestFirstPartyModels,
  splitCode,
  sortProvidersFirstPartyFirst,
} from "@/lib/content/models";
import { Icon } from "@/components/icon";
import { ModelBadge, ModelsCatalog } from "@/components/models-catalog";
import { PageHeader, Section } from "@/components/page-header";
import type { LocalizedText } from "@/lib/content/vocabulary";
import { fill, pickText } from "@/lib/i18n/dictionaries";
import { bodyDateLocale } from "@/lib/i18n/fallback";
import { FallbackRegion } from "@/components/fallback-region";

export const revalidate = 300;

function Rich({ text }: { text: string }) {
  return <>{splitCode(text).map((part, i) => (part.code ? <code key={i}>{part.text}</code> : part.text))}</>;
}

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return buildPageMetadata({ path: "/models", locale,
    title: pickText(MODELS_COPY.metaTitle, locale),
    description: pickText(MODELS_COPY.metaDescription, locale) });
}

export default async function ModelsPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const facts = await getFacts();
  const released = releaseView(facts);
  const t = (copy: LocalizedText) => fill(pickText(copy, locale), {
    version: facts.latestPublishedRelease?.version ?? "—", model: facts.defaultModel ?? "—", count: released.models.length,
  });
  const dateLocale = bodyDateLocale("/models", locale);
  const latest = latestFirstPartyModels(released.models);
  const providers = sortProvidersFirstPartyFirst(released.providers);
  const routeEnv = ROUTE_PROVIDERS.flatMap((route) => {
    const provider = released.providers.find((p) => p.id === route.id);
    return provider ? [{ ...route, env: provider.env }] : [];
  });
  const deepseekEnv = released.providers.find((p) => p.id === "deepseek")?.env;

  return (
    <FallbackRegion path="/models" locale={locale}>
      <PageHeader
        titleId="models-overview"
        title={t(MODELS_COPY.title)}
        lede={t(MODELS_COPY.lead)}
        pose="think"
      />

      <div className="page-body">
        {latest.length > 0 ? (
          <Section id="models-latest" title={t(MODELS_COPY.latestTitle)} scope={t(MODELS_COPY.latestLead)}>
            <ul className="models-latest">
              {latest.map(({ key, label, model }) => (
                <li key={key} className="tile">
                  <span className="models-maker">{label}</span>
                  <code>{model.id}</code>
                  <span className="models-meta">
                    {formatModelTokens(model.contextWindow)} {t(MODELS_COPY.colContext).toLowerCase()}
                    {model.reasoning ? <>{" "}<ModelBadge locale={locale} /></> : null}
                  </span>
                  <span className="models-meta">
                    {fill(pickText(MODELS_COPY.latestSince, locale), { date: formatModelDate(model.addedAt, dateLocale) })}
                  </span>
                </li>
              ))}
            </ul>
          </Section>
        ) : null}

        <Section
          id="models-setup"
          title={t(MODELS_COPY.setupTitle)}
          scope={t(MODELS_COPY.setupLead)}
          link={
            <Link href="https://github.com/codewhale-hq/CodeWhale/blob/main/docs/PROVIDERS.md" className="section-link">
              {t(MODELS_COPY.providerDocs)}
              <Icon name="external" className="icon" />
            </Link>
          }
        >
          <ol className="steps">
            {MODELS_COPY.steps.map((step, i) => (
              <li key={i}>
                <span className="gs-step-index" aria-hidden="true">{i + 1}</span>
                <h3>{t(step.title)}</h3>
                <p><Rich text={t(step.detail)} /></p>
              </li>
            ))}
          </ol>
          <div className="models-routes">
            <div className="tile">
              <h3 className="models-route-title">{t(MODELS_COPY.routeDeepseekTitle)}</h3>
              <p>{t(MODELS_COPY.routeDeepseekBody)}</p>
              {deepseekEnv ? <code>{deepseekEnv}</code> : null}
            </div>
            <div className="tile">
              <h3 className="models-route-title">{t(MODELS_COPY.routeFirstPartyTitle)}</h3>
              <p>{t(MODELS_COPY.routeFirstPartyBody)}</p>
              <dl className="models-env">
                {routeEnv.map((route) => (
                  <div key={route.id}>
                    <dt>{route.label}</dt>
                    <dd><code>{route.env}</code></dd>
                  </div>
                ))}
              </dl>
            </div>
            <div className="tile">
              <h3 className="models-route-title">{t(MODELS_COPY.routeCompatTitle)}</h3>
              <p>{t(MODELS_COPY.routeCompatBody)}</p>
              <p><Rich text={t(MODELS_COPY.routeCompatConfig)} /></p>
            </div>
          </div>
        </Section>

        {released.counts && (
          <Section id="models-title" title={t(MODELS_COPY.modelsTitle)} scope={t(MODELS_COPY.modelsLead)}>
            <ModelsCatalog models={released.models} locale={locale} />
          </Section>
        )}

        <Section id="providers-title" title={t(MODELS_COPY.listTitle)}>
          <details className="group-card disclosure-row">
            <summary>
              <span>{released.counts ? fill(pickText(MODELS_COPY.providerCount, locale), { count: released.counts.providerRoutes }) : t(MODELS_COPY.listTitle)}</span>
              <Icon name="chevron-down" className="disclosure-chevron" />
            </summary>
            <div className="disclosure-body">
              <div className="data-table-wrap">
                <table className="data-table">
                                    <thead>
                    <tr>
                      <th scope="col">{t(MODELS_COPY.provider)}</th>
                      <th scope="col">{t(MODELS_COPY.id)}</th>
                      <th scope="col">{t(MODELS_COPY.credential)}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {providers.map((provider) => (
                      <tr key={provider.id}>
                        <th scope="row">{provider.label}</th>
                        <td><code>{provider.id}</code></td>
                        <td><code>{provider.env}</code></td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          </details>
          <p className="section-scope mt-5">
            {t(MODELS_COPY.missing)}{" "}
            <Link href="https://github.com/codewhale-hq/CodeWhale/issues/new/choose" className="link">{t(MODELS_COPY.request)}</Link>
          </p>
        </Section>
      </div>
    </FallbackRegion>
  );
}
