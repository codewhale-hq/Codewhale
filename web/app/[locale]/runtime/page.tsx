import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { Status } from "@/components/status-badge";
import { releaseView } from "@/lib/capabilities";
import { getFacts } from "@/lib/facts";
import { getChrome, getRuntime, pickText } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

import { INTEGRATIONS, TRUST, REPO_BLOB_BASE } from "@/lib/content/runtime-integrations";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getRuntime(locale);
  return buildPageMetadata({
    path: "/runtime",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function RuntimePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const han = locale === "zh";
  const chrome = getChrome(locale);
  const t = getRuntime(locale);
  const facts = await getFacts();
  const releasedTools = releaseView(facts).counts?.tools ?? null;

  return (
    <>
      <PageHeader
        seal={han ? "接" : undefined}
        title={t.title}
        titleAside={han ? t.titleAside : undefined}
        titleAsideLang={han ? t.titleAsideLang : undefined}
        lede={t.lede}
        pose="run"
        actions={
          <>
            <Link href={`/${locale}/plugins`} className="btn btn-primary btn-lg">
              {chrome.navPlugins}
            </Link>
            <Link href={`/${locale}/docs/mcp`} className="btn btn-secondary btn-lg">
              {t.mcpDoc}
            </Link>
          </>
        }
      />

      <div className="page-body">
        <Section id="runtime-integrations" seal={han ? "集" : undefined} title={t.integrationsTitle}>
          <ul className="dir-list dir-list-card" role="list">
            {INTEGRATIONS.map((item) => (
              <li key={item.name}>
                <a href={item.href} target="_blank" rel="noopener noreferrer" className="dir-row">
                  <span className="dir-mark" aria-hidden="true"><Icon name={item.icon} /></span>
                  <span className="dir-text">
                    <span className="dir-title">{item.name}</span>
                    <span className="dir-purpose">{pickText(item.desc, locale)}</span>
                  </span>
                  {item.experimental ? <Status tone="attention">{t.experimental}</Status> : null}
                  <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                </a>
              </li>
            ))}
          </ul>
        </Section>

        <Section id="runtime-trust" layout="split" seal={han ? "信" : undefined} title={t.trustTitle}>
          <dl className="ruled-list ruled-list-stacked">
            {TRUST.map((item) => (
              <div key={item.title.en}>
                <dt>{pickText(item.title, locale)}</dt>
                <dd>{pickText(item.body, locale)}</dd>
              </div>
            ))}
          </dl>
        </Section>

        <Section id="runtime-facts" layout="split" seal={han ? "数" : undefined} title={t.factsTitle}>
          <dl className="ruled-list">
            <div>
              <dt>{t.version}</dt>
              <dd className="tabular">{facts.version ?? "—"}</dd>
            </div>
            {releasedTools !== null && (
              <div>
                <dt>{t.toolCount}</dt>
                <dd className="tabular">{releasedTools}</dd>
              </div>
            )}
            <div>
              <dt>{t.sandboxBackends}</dt>
              <dd>{facts.sandboxBackends.length ? facts.sandboxBackends.join(" · ") : "—"}</dd>
            </div>
          </dl>
          {/* Crate names and the source revision are for maintainers; they
              sit behind Details. */}
          <details className="group-card disclosure-row mt-4">
            <summary>
              {t.details}
              <Icon name="chevron-down" className="disclosure-chevron" />
            </summary>
            <dl className="disclosure-body runtime-details">
              <div>
                <dt>Crates</dt>
                <dd>{facts.crates.length ? `${facts.crates.length} · ${facts.crates.join(", ")}` : "—"}</dd>
              </div>
              <div>
                <dt>{t.sourceRevision}</dt>
                <dd><code className="inline">{facts.sourceRevision ?? "—"}</code></dd>
              </div>
            </dl>
          </details>
        </Section>

        <section className="page-section">
          <p className="status-line">
            <span>{t.docsLead}</span>
            <a href={`${REPO_BLOB_BASE}/docs/RUNTIME_API.md`} target="_blank" rel="noopener noreferrer" className="link">
              {t.runtimeApiDoc}
            </a>
            <a href={`${REPO_BLOB_BASE}/docs/MCP.md`} target="_blank" rel="noopener noreferrer" className="link">
              {t.mcpDoc}
            </a>
          </p>
        </section>
      </div>
    </>
  );
}
