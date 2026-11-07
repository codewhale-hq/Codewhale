import Link from "next/link";
import { PageHeader, Section } from "@/components/page-header";
import { Status, type StatusTone } from "@/components/status-badge";
import { NativeTerminalGallery } from "@/components/native-terminal-gallery";
import { PRODUCT_COPY } from "@/lib/content/product";
import { fill, getChrome, getHome, pickText } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";
import { getNativeTerminalCopy } from "@/lib/content/native-terminal";
import { TERMINAL_SCREENSHOT } from "@/lib/media-manifest";

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

// Terminal released · local browser ships with it · CodeWhale GUI
// available · hosted web in development · desktop development build · Mac beta.
const AVAILABILITY_TONES: StatusTone[] = ["ready", "ready", "ready", "idle", "idle", "attention"];

/**
 * /product — what Codewhale is and what a person gains, with availability
 * stated per surface. Shared copy is the `PRODUCT_COPY` content module.
 */
export default async function ProductPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = (text: { en: string; zh: string }) => pickText(text, locale);
  const home = getHome(locale);
  const terminal = getNativeTerminalCopy(locale);

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
            {PRODUCT_COPY.workPatterns.map((pattern) => (
              <div key={pattern.href}>
                <dt>{t(pattern.title)}</dt>
                <dd>{t(pattern.body)} <Link className="section-link" href={pattern.href.startsWith("/") ? `/${locale}${pattern.href}` : pattern.href}><code>{pattern.command}</code></Link></dd>
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
              {PRODUCT_COPY.modes.map((row) => (
                <div key={row.title.en}>
                  <dt>{t(row.title)}</dt>
                  <dd>{t(row.body)}</dd>
                </div>
              ))}
            </dl>
            <dl className="ruled-list">
              {PRODUCT_COPY.permissions.map((row) => (
                <div key={row.title.en}>
                  <dt>{t(row.title)}</dt>
                  <dd>{t(row.body)}</dd>
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
            {PRODUCT_COPY.availability.map((row, index) => (
              <div key={row.surface.en}>
                <dt>{t(row.surface)}</dt>
                <dd>
                  <Status tone={AVAILABILITY_TONES[index] ?? "idle"}>{t(row.status)}</Status>
                  {t(row.detail)}
                  {row.href && row.linkLabel && (
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
            ))}
          </dl>
        </Section>
      </div>
    </>
  );
}
