import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { Status } from "@/components/status-badge";
import { getComputerUse, getFeatures } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";
import { COMPUTER_USE_REPO, getComputerUseRelease } from "@/lib/computer-use-release";
import { getEnv } from "@/lib/kv";
import { FACTS } from "@/lib/facts.generated";
import { EnglishText, FallbackRegion } from "@/components/fallback-region";

const BUNDLED_CU_CAVEAT_VERSION = "0.10.2";

// Rendered per request rather than through ISR: the download state must be
// right the moment a release is published, the page must never serve a
// build-time snapshot, and OpenNext's in-isolate revalidation queue was seen
// leaving stale copies in place for many minutes. The GitHub reads inside
// getComputerUseRelease stay cached for five minutes via the fetch data cache.
export const dynamic = "force-dynamic";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const copy = getComputerUse(locale);
  return buildPageMetadata({ path: "/computer-use", locale, title: copy.metaTitle, description: copy.metaDescription });
}

/**
 * /computer-use — the Mac helper. The download button exists only while the
 * release check reports a ready, verified release; otherwise the page says
 * why there is no download and links the published releases.
 */
export default async function ComputerUsePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getComputerUse(locale);
  const release = await getComputerUseRelease((await getEnv()).GITHUB_TOKEN);
  // The disk image is the human download; the archive stays for the updater.
  const primary = release.status === "ready" ? (release.dmg ?? { downloadUrl: release.downloadUrl, size: release.size }) : null;

  return (
    <FallbackRegion path="/computer-use" locale={locale}>
      <PageHeader
        title={t.title}
        lede={t.lead}
        pose="computer"
        meta={
          <>
            <Status tone="attention">{getFeatures(locale).preview}</Status>
            <br />
            {t.requirements}
          </>
        }
        actions={
          <>
            {release.status === "ready" && primary ? (
              <a className="btn btn-primary btn-lg" href={primary.downloadUrl}>
                {t.download}
              </a>
            ) : null}
            <a href={COMPUTER_USE_REPO} className="btn btn-secondary btn-lg">
              {t.source}
              <Icon name="external" className="icon" />
            </a>
          </>
        }
      >
        {release.status === "ready" && primary ? (
          <div className="cu-download">
            <span className="page-meta tabular">
              v{release.version} · {Math.ceil(primary.size / 1024 / 1024)} MB
            </span>
            <p className="status-line">
              <a href={release.receiptUrl} className="link">{t.receipt}</a>
              {release.dmg ? <a href={release.downloadUrl} className="link">{t.downloadZip}</a> : null}
            </p>
          </div>
        ) : (
          <div className="callout" role="status">
            <p className="callout-title">
              <Status tone={release.status === "pending" ? "attention" : "idle"}>
                {release.status === "pending" ? t.pendingTitle : t.unavailableTitle}
              </Status>
            </p>
            <p>{release.status === "pending" ? t.pendingBody : t.unavailableBody}</p>
            <p>
              <a href={`${COMPUTER_USE_REPO}/releases`} className="link">{t.releases}</a>
            </p>
          </div>
        )}
      </PageHeader>

      <div className="page-body">
        <Section id="cu-setup" title={t.setup}>
          <ol className="steps">
            {t.steps.map((step, index) => (
              <li key={index}>
                <span className="gs-step-index" aria-hidden="true">{index + 1}</span>
                <h3>{step.title}</h3>
                <p>{step.body}</p>
              </li>
            ))}
          </ol>
        </Section>

        <Section id="cu-codewhale" title={t.connectTitle}>
          {FACTS.latestPublishedRelease?.version === BUNDLED_CU_CAVEAT_VERSION && <p>{t.connectVersion}</p>}
          <p>{t.connectLimit}</p>
          <p>{t.connectRuntime}</p>
          <pre tabIndex={0} className="code-block">{`APP="/Applications/Codewhale Computer Use.app"
codewhale mcp add computer --command "$APP/Contents/MacOS/node" --arg "$APP/Contents/Resources/plugin/mcp/server.mjs"
codewhale mcp tools computer`}</pre>
          <p>{t.connectReload}</p>
          <pre tabIndex={0} className="code-block">{t.connectPrompt}</pre>
          <p>{t.connectExpected}</p>
          <p className="status-line">
            <Link href={`/${locale}/docs/mcp`} className="link">{t.connectGuide}</Link>
            <a href={`${COMPUTER_USE_REPO}/blob/main/README.md#codewhale-cli-setup`} className="link"><EnglishText>Computer Use</EnglishText></a>
          </p>
        </Section>

        <section className="page-section" aria-label={t.controlsTitle}>
          <div className="grid-2">
            <div className="tile">
              <span className="tile-icon" aria-hidden="true"><Icon name="shield" /></span>
              <h2 className="section-title">{t.controlsTitle}</h2>
              <p>{t.controlsBody}</p>
              <a href={`${COMPUTER_USE_REPO}/blob/main/docs/DEMO.md`} className="section-link">
                {t.demo}
                <Icon name="arrow-right" className="icon icon-flip" />
              </a>
            </div>
            <div className="tile">
              <span className="tile-icon" aria-hidden="true"><Icon name="repeat" /></span>
              <h2 className="section-title">{t.updateTitle}</h2>
              <p>{t.updateBody}</p>
              <a href={`${COMPUTER_USE_REPO}/blob/main/CHANGELOG.md`} className="section-link">
                {t.notes}
                <Icon name="arrow-right" className="icon icon-flip" />
              </a>
            </div>
          </div>
        </section>

        <section className="page-section">
          <div className="callout">
            <p className="callout-title">
              <Icon name="info" className="icon" />
              {t.platforms}
            </p>
            <div className="actions">
              <a href={`${COMPUTER_USE_REPO}/blob/main/docs/TROUBLESHOOTING.md`} className="btn btn-secondary">{t.help}</a>
            </div>
          </div>
        </section>

        <Section id="cu-contribute" title={t.contributeTitle}>
          <p>{t.contributeBody}</p>
          <div className="actions">
            <a href={`${COMPUTER_USE_REPO}/blob/main/CONTRIBUTING.md`} className="btn btn-secondary">{t.contributeGuide}</a>
            <a href={`${COMPUTER_USE_REPO}/issues`} className="btn btn-ghost">{t.contributeIssue}</a>
          </div>
        </Section>
      </div>
    </FallbackRegion>
  );
}
