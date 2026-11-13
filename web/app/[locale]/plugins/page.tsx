import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { buildPageMetadata } from "@/lib/page-meta";
import { PLUGINS_COPY, PLUGINS_COMING, pluginsComingApplies, PLUGIN_KINDS, FEATURED_CONNECTORS, CATALOG_FEATURED, PLUGIN_SOURCES, PLUGIN_COMMANDS, PLUGIN_COMPATIBILITY } from "@/lib/content/plugins";
import { CATALOG_PLUGINS, CONNECTORS, type ConnectorAuth } from "@/lib/content/connectors.generated";
import type { LocalizedText } from "@/lib/content/vocabulary";
import { getFacts } from "@/lib/facts";
import { pickText } from "@/lib/i18n/dictionaries";
import { FallbackRegion } from "@/components/fallback-region";

export const revalidate = 300;

const MARKETPLACE_REPO = "https://github.com/codewhale-hq/codewhale-plugin-marketplace";
const DSH_REPO = "https://github.com/deepseek-ai/deepseek-harness";
const DSH_COMPAT_DOC = "https://github.com/codewhale-hq/Codewhale/blob/main/docs/DSH_PLUGIN_COMPAT.md";

const AUTH_LABEL: Record<ConnectorAuth, LocalizedText> = {
  oauth: PLUGINS_COPY.authOauth,
  token: PLUGINS_COPY.authToken,
  none: PLUGINS_COPY.authNone,
};

const featuredConnectors = FEATURED_CONNECTORS.flatMap((id) => CONNECTORS.find((connector) => connector.id === id) ?? []);
const connectorCommands = "codewhale mcp add notion --url https://mcp.notion.com/mcp\ncodewhale mcp login notion";
const pluginVersions = new Map(CATALOG_PLUGINS.map((plugin) => [plugin.name, plugin.version]));

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return buildPageMetadata({ path: "/plugins", locale,
    title: pickText(PLUGINS_COPY.metaTitle, locale),
    description: pickText(PLUGINS_COPY.metaDescription, locale) });
}

export default async function PluginsPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = (copy: LocalizedText) => pickText(copy, locale);
  const showComing = pluginsComingApplies((await getFacts()).latestPublishedRelease);
  const authoringDoc = `/${locale}/docs/plugin-authoring`;
  const localHref = (href: string) => (href.startsWith("/") ? `/${locale}${href}` : href);
  const moreConnectors = CONNECTORS.length - featuredConnectors.length;
  const moreCatalog = CATALOG_PLUGINS.length - CATALOG_FEATURED.length;

  return (
    <FallbackRegion path="/plugins" locale={locale}>
      <PageHeader
        titleId="plugins-overview"
        title={t(PLUGINS_COPY.title)}
        lede={t(PLUGINS_COPY.lead)}
        pose="connect"
        actions={
          <>
            <Link href="#catalog-title" className="btn btn-primary btn-lg">{t(PLUGINS_COPY.marketplaceCta)}</Link>
            <Link href={authoringDoc} className="btn btn-secondary btn-lg">{t(PLUGINS_COPY.docsCta)}</Link>
          </>
        }
      />

      <div className="page-body">
        <Section id="kinds-title" title={t(PLUGINS_COPY.kindsTitle)}>
          <ul className="plugin-kinds" role="list">
            {PLUGIN_KINDS.map((kind) => (
              <li key={kind.title.en}>
                <h3>{t(kind.title)}</h3>
                <p>{t(kind.detail)}</p>
              </li>
            ))}
          </ul>
          <div className="stack mt-4">
            <p>
              {t(PLUGINS_COPY.originBefore)}
              <Link href={DSH_REPO} className="link">{t(PLUGINS_COPY.originName)}</Link>
              {t(PLUGINS_COPY.originAfter)}
            </p>
            <p className="section-scope">{t(PLUGINS_COPY.dshStatus)}</p>
            <Link href={DSH_COMPAT_DOC} className="section-link">
              {t(PLUGINS_COPY.dshLink)}
              <Icon name="external" className="icon" />
            </Link>
          </div>
        </Section>

        {showComing ? (
          <Section id="coming-title" title={t(PLUGINS_COPY.comingTitle)} scope={t(PLUGINS_COPY.comingLead)}>
            <ul className="plugin-kinds plugin-kinds-coming" role="list" data-release-scope="coming">
              {PLUGINS_COMING.map((item) => (
                <li key={item.title.en}>
                  <h3>{t(item.title)}</h3>
                  <p>{t(item.detail)}</p>
                </li>
              ))}
            </ul>
          </Section>
        ) : null}

        <Section id="connectors-title" title={t(PLUGINS_COPY.connectorsTitle)} scope={t(PLUGINS_COPY.connectorsLead)}>
          <ul className="connector-grid" role="list">
            {featuredConnectors.map((connector) => (
              <li key={connector.id}>
                <span className="connector-name">{connector.name}</span>
                <span className={`connector-auth connector-auth-${connector.auth}`}>{t(AUTH_LABEL[connector.auth])}</span>
              </li>
            ))}
            <li className="connector-more">
              <Link href={`${MARKETPLACE_REPO}/blob/main/connections/catalog.json`} className="link">
                {`+${moreConnectors} ${t(PLUGINS_COPY.connectorsMore)}`}
              </Link>
            </li>
          </ul>
          <pre tabIndex={0} className="code-block mt-4">{connectorCommands}</pre>
          <div className="stack mt-4">
            <p className="section-scope">{t(PLUGINS_COPY.connectorsNote)}</p>
            <p className="section-scope">{t(PLUGINS_COPY.connectorsGoogle)}</p>
            <Link href={`/${locale}/docs/mcp`} className="section-link">
              {t(PLUGINS_COPY.connectorsLink)}
              <Icon name="arrow-right" className="icon icon-flip" />
            </Link>
          </div>
        </Section>

        <Section id="catalog-title" title={t(PLUGINS_COPY.catalogTitle)} scope={t(PLUGINS_COPY.catalogLead)}>
          <ul className="dir-list dir-list-card" role="list">
            {CATALOG_FEATURED.map((entry) => {
              const version = pluginVersions.get(entry.name);
              const flagship = entry.name === "computer-use";
              const href = flagship ? `/${locale}/computer-use` : `${MARKETPLACE_REPO}/tree/main/plugins/${entry.name}`;
              return (
                <li key={entry.name}>
                  <Link href={href} className="dir-row">
                    <span className="dir-mark" aria-hidden="true"><Icon name="plug" /></span>
                    <span className="dir-text">
                      <span className="dir-title">
                        {entry.title}
                        {version ? <span className="plugin-version">{` v${version}`}</span> : null}
                        {entry.status ? <span className="plugin-version">{` · ${t(entry.status)}`}</span> : null}
                      </span>
                      <span className="dir-purpose">{t(entry.detail)}</span>
                      {flagship ? null : <span className="dir-meta">{`/plugin marketplace install codewhale ${entry.name}`}</span>}
                    </span>
                    <span className="dir-action" aria-hidden="true"><Icon name={flagship ? "arrow-right" : "external"} className={flagship ? "icon-flip" : undefined} /></span>
                  </Link>
                </li>
              );
            })}
          </ul>
          <Link href={MARKETPLACE_REPO} className="section-link mt-4">
            {`${t(PLUGINS_COPY.catalogSeeAll)} (+${moreCatalog})`}
            <Icon name="external" className="icon" />
          </Link>
        </Section>

        <Section id="safe-title" title={t(PLUGINS_COPY.safeTitle)} scope={t(PLUGINS_COPY.safeLead)}>
          <ol className="plugin-flow">
            {PLUGINS_COPY.safeSteps.map((step, index) => (
              <li key={step.command}>
                <span className="plugin-flow-title"><span className="gs-step-index" aria-hidden="true">{index + 1}</span>{t(step.title)}</span>
                <code className="plugin-flow-command">{step.command}</code>
                <span>{t(step.detail)}</span>
              </li>
            ))}
          </ol>
          <p className="section-scope mt-4">{t(PLUGINS_COPY.safeHashes)}</p>
          <p className="section-scope mt-4">{t(PLUGINS_COPY.sourcesLead)}</p>
          <pre tabIndex={0} className="code-block mt-2">{PLUGIN_SOURCES}</pre>
        </Section>

        <section className="page-section" aria-labelledby="reference-title">
          <details className="group-card disclosure-row">
            <summary>
              <span id="reference-title">{t(PLUGINS_COPY.referenceTitle)}</span>
              <Icon name="chevron-down" className="disclosure-chevron" />
            </summary>
            <div className="disclosure-body stack">
              <h3 className="section-title">{t(PLUGINS_COPY.referenceCommands)}</h3>
              <pre tabIndex={0} className="code-block">{PLUGIN_COMMANDS.join("\n")}</pre>
              <h3 className="section-title">{t(PLUGINS_COPY.referenceFormats)}</h3>
              <dl className="def-rows">
                {PLUGIN_COMPATIBILITY.map((format) => (
                  <div key={format.href} className="def-row">
                    <dt><Link className="link" href={localHref(format.href)}>{t(format.title)}</Link></dt>
                    <dd>{t(format.detail)}</dd>
                  </div>
                ))}
              </dl>
            </div>
          </details>
        </section>

        <Section id="contribute-title" title={t(PLUGINS_COPY.contributeTitle)} scope={t(PLUGINS_COPY.contributeLead)}>
          <div className="actions">
            <Link href={authoringDoc} className="btn btn-secondary">{t(PLUGINS_COPY.docsCta)}</Link>
            <Link href={`${MARKETPLACE_REPO}/issues`} className="section-link">{t(PLUGINS_COPY.contributeLink)}<Icon name="external" className="icon" /></Link>
          </div>
        </Section>
      </div>
    </FallbackRegion>
  );
}
