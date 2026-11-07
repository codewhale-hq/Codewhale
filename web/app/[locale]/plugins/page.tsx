import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { buildPageMetadata } from "@/lib/page-meta";
import { PLUGINS_COPY, PLUGIN_COMMANDS } from "@/lib/content/plugins";
import type { LocalizedText } from "@/lib/content/vocabulary";
import { pickText } from "@/lib/i18n/dictionaries";

export const revalidate = 300;

const MARKETPLACE_REPO = "https://github.com/codewhale-hq/codewhale-plugin-marketplace";
const AUTHORING_DOC = "https://github.com/codewhale-hq/CodeWhale/blob/main/docs/PLUGIN_AUTHORING.md";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return buildPageMetadata({ path: "/plugins", locale,
    title: pickText(PLUGINS_COPY.metaTitle, locale),
    description: pickText(PLUGINS_COPY.metaDescription, locale) });
}

/**
 * /plugins — what a plugin adds, the marketplace catalogue, where plugins
 * come from, how trust works, and the commands. Nothing runs until the
 * reader reviews, trusts and enables it; the page says so where it matters.
 */
export default async function PluginsPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = (copy: LocalizedText) => pickText(copy, locale);
  const authoringLink = (
    <Link href={AUTHORING_DOC} className="section-link">
      {t(PLUGINS_COPY.docsCta)}
      <Icon name="external" className="icon" />
    </Link>
  );

  return (
    <>
      <PageHeader
        title={t(PLUGINS_COPY.title)}
        lede={t(PLUGINS_COPY.lead)}
        pose="connect"
        actions={
          <>
            <Link href={MARKETPLACE_REPO} className="btn btn-primary btn-lg">{t(PLUGINS_COPY.marketplaceCta)}</Link>
            <Link href={AUTHORING_DOC} className="btn btn-secondary btn-lg">{t(PLUGINS_COPY.docsCta)}</Link>
          </>
        }
      />

      <div className="page-body">
        <Section id="cu-title" title={t(PLUGINS_COPY.cuTitle)} scope={t(PLUGINS_COPY.cuLead)}>
          <div className="split">
            <div className="stack">
              <pre tabIndex={0} className="code-block">{t(PLUGINS_COPY.cuInstall)}</pre>
              <p className="section-scope">{t(PLUGINS_COPY.cuToolsNote)}</p>
              <Link href={`/${locale}/computer-use`} className="section-link">
                Computer Use
                <Icon name="arrow-right" className="icon icon-flip" />
              </Link>
            </div>
            <dl className="def-rows">
              {PLUGINS_COPY.cuFeatures.map((feature) => (
                <div key={t(feature.title)} className="def-row">
                  <dt>{t(feature.title)}</dt>
                  <dd>{t(feature.detail)}</dd>
                </div>
              ))}
            </dl>
          </div>
        </Section>

        <Section id="catalog-title" title={t(PLUGINS_COPY.catalogTitle)} scope={t(PLUGINS_COPY.catalogLead)}>
          <pre tabIndex={0} className="code-block mb-4">{t(PLUGINS_COPY.catalogBrowse)}</pre>
          <ul className="dir-list dir-list-card" role="list">
            {PLUGINS_COPY.catalogEntries.map((entry) => (
              <li key={entry.reference}>
                <Link href={MARKETPLACE_REPO} className="dir-row">
                  <span className="dir-mark" aria-hidden="true"><Icon name="plug" /></span>
                  <span className="dir-text">
                    <span className="dir-title">{t(entry.title)}</span>
                    <span className="dir-purpose">{t(entry.detail)}</span>
                    <span className="dir-meta">{entry.reference}</span>
                  </span>
                  <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                </Link>
              </li>
            ))}
          </ul>
          <p className="section-scope mt-4">{t(PLUGINS_COPY.catalogNote)}</p>
        </Section>

        <Section id="sources-title" title={t(PLUGINS_COPY.sourcesTitle)} scope={t(PLUGINS_COPY.sourcesLead)}>
          <ul className="dir-list dir-list-card" role="list">
            {PLUGINS_COPY.sources.map((source) => (
              <li key={source.reference}>
                <div className="dir-row">
                  <span className="dir-mark" aria-hidden="true"><Icon name="package" /></span>
                  <span className="dir-text">
                    <span className="dir-title">{t(source.title)}</span>
                    <span className="dir-purpose">{t(source.detail)}</span>
                    <span className="dir-meta">{source.reference}</span>
                  </span>
                </div>
              </li>
            ))}
          </ul>
        </Section>

        <Section id="trust-title" title={t(PLUGINS_COPY.trustTitle)} scope={t(PLUGINS_COPY.trustLead)}>
          <ol className="steps">
            {PLUGINS_COPY.trustSteps.map((step, index) => (
              <li key={t(step.title)}>
                <span className="gs-step-index" aria-hidden="true">{index + 1}</span>
                <h3>{t(step.title)}</h3>
                <p>{t(step.detail)}</p>
              </li>
            ))}
          </ol>
          <p className="section-scope mt-4">{t(PLUGINS_COPY.trustNote)}</p>
        </Section>

        <Section id="commands-title" title={t(PLUGINS_COPY.commandsTitle)} scope={t(PLUGINS_COPY.commandsLead)} link={authoringLink}>
          <pre tabIndex={0} className="code-block">{PLUGIN_COMMANDS.join("\n")}</pre>
        </Section>
      </div>
    </>
  );
}
