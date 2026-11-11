import Link from "next/link";
import { PageHeader, Section } from "@/components/page-header";
import { Status } from "@/components/status-badge";
import { NativeTerminalGallery } from "@/components/native-terminal-gallery";
import { DesktopDownloadBlock } from "@/components/desktop-download";
import { claimedItems, runtimeCapabilityContext } from "@/lib/capabilities";
import { describeRow } from "@/lib/content/availability";
import { PRODUCT_COPY } from "@/lib/content/product";
import { getFacts } from "@/lib/facts";
import { fill, getChrome, getHome, pickText } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";
import { getNativeTerminalCopy } from "@/lib/content/native-terminal";
import { TERMINAL_SCREENSHOT } from "@/lib/media-manifest";
import { stateLabel } from "@/lib/surface-states";

export const revalidate = 300;

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return buildPageMetadata({
    path: "/product",
    locale,
    title: pickText(PRODUCT_COPY.metadata.title, locale),
    description: pickText(PRODUCT_COPY.metadata.description, locale),
  });
}

/**
 * /product — what Codewhale is and what a person gains, with availability
 * stated per surface. Shared copy is the `PRODUCT_COPY` content module.
 */
export default async function ProductPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = (text: { en: string; zh: string }) => pickText(text, locale);
  const home = getHome(locale);
  const terminal = getNativeTerminalCopy(locale);
  const capabilityContext = runtimeCapabilityContext(await getFacts());
  const inSource = <Status tone="idle">{stateLabel("in_source", locale)}</Status>;

  return (
    <>
      <PageHeader
        title={t(PRODUCT_COPY.title)}
        lede={t(PRODUCT_COPY.lede)}
        pose="think"
        actions={
          <>
            <Link href={`/${locale}/install`} className="btn btn-primary btn-lg">
              {t(PRODUCT_COPY.actions.install)}
            </Link>
            <Link href={`/${locale}/docs`} className="btn btn-secondary btn-lg">
              {t(PRODUCT_COPY.actions.docs)}
            </Link>
            <Link href={`/${locale}/features`} className="btn btn-secondary btn-lg">{getChrome(locale).navFeatures}</Link>
          </>
        }
      />

      <div className="page-body">
        <Section id="product-terminal" title={terminal.title} scope={terminal.description}>
          <div className="figure-frame">
            <NativeTerminalGallery
              locale={locale}
              defaultFrame="composer"
              regionLabel={home.shotPreview}
              label={fill(home.screenshotAlt, { version: TERMINAL_SCREENSHOT.version })}
            />
          </div>
        </Section>

        <Section id="product-workflows" layout="split" title={t(PRODUCT_COPY.workHeading)}>
          <dl className="ruled-list">
            {claimedItems(PRODUCT_COPY.workPatterns, capabilityContext).map(({ item: pattern, availability }) => (
              <div key={pattern.href}>
                <dt>{t(pattern.title)}</dt>
                <dd>{availability === "in_source" ? inSource : null}{t(pattern.body)} <Link className="section-link" href={pattern.href.startsWith("/") ? `/${locale}${pattern.href}` : pattern.href}><code>{pattern.command}</code></Link></dd>
              </div>
            ))}
          </dl>
        </Section>

        <Section
          id="product-control"
          layout="split"
          title={t(PRODUCT_COPY.controlHeading)}
        >
          <div className="stack">
            <dl className="ruled-list">
              {claimedItems(PRODUCT_COPY.modes, capabilityContext).map(({ item: row, availability }) => (
                <div key={row.title.en}>
                  <dt>{t(row.title)}</dt>
                  <dd>{availability === "in_source" ? inSource : null}{t(row.body)}</dd>
                </div>
              ))}
            </dl>
            <dl className="ruled-list">
              {claimedItems(PRODUCT_COPY.permissions, capabilityContext).map(({ item: row, availability }) => (
                <div key={row.title.en}>
                  <dt>{t(row.title)}</dt>
                  <dd>{availability === "in_source" ? inSource : null}{t(row.body)}</dd>
                </div>
              ))}
            </dl>
          </div>
        </Section>

        <Section
          id="product-availability"
          layout="split"
          title={t(PRODUCT_COPY.availabilityHeading)}
        >
          <dl className="ruled-list">
            {PRODUCT_COPY.availability.map((row) => {
              const view = describeRow(row, locale);
              return (
                <div key={row.id}>
                  <dt>{t(row.surface)}</dt>
                  <dd>
                    <Status tone={view.tone}>{view.status}</Status>
                    {view.detail}
                    {view.download ? <DesktopDownloadBlock download={view.download} locale={locale} /> : null}
                    {view.link && row.href && row.linkLabel && (
                      <>
                        {" "}
                        {row.href.startsWith("http") ? (
                          <a
                            href={row.href}
                            className="link"
                            target="_blank"
                            rel="noopener noreferrer"
                          >
                            {t(row.linkLabel)}
                          </a>
                        ) : (
                          <Link href={`/${locale}${row.href}`} className="link">
                            {t(row.linkLabel)}
                          </Link>
                        )}
                      </>
                    )}
                  </dd>
                </div>
              );
            })}
          </dl>
        </Section>
      </div>
    </>
  );
}
