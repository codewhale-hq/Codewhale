import Image from "next/image";
import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { Status } from "@/components/status-badge";
import { DesktopDownloadBlock } from "@/components/desktop-download";
import { AVAILABILITY_ROWS, describeRow } from "@/lib/content/availability";
import { APP_COPY } from "@/lib/content/app-page";
import { pickText } from "@/lib/i18n/dictionaries";
import { stateLabel, STATE_TONE, surfaceState } from "@/lib/surface-states";
import { buildPageMetadata } from "@/lib/page-meta";

export const revalidate = 300;

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return buildPageMetadata({
    path: "/app",
    locale,
    title: pickText(APP_COPY.metadata.title, locale),
    description: pickText(APP_COPY.metadata.description, locale),
  });
}

/**
 * /app — the desktop app and the hosted browser app, stated as they are
 * today: a development build and a surface that is not open yet. Copy lives
 * in lib/content/app-page.ts and the surface table in
 * lib/content/availability.ts, which /product reads too.
 */
export default async function AppPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = (text: { en: string; zh: string }) => pickText(text, locale);
  const rows = AVAILABILITY_ROWS.filter((row) => row.pages.includes("app"));
  const desktopState = surfaceState("desktop");
  const hostedState = surfaceState("hosted-web");

  return (
    <>
      <PageHeader
        title={t(APP_COPY.title)}
        lede={t(APP_COPY.lede)}
        pose="think"
        meta={
          <p className="app-status">
            <Status tone={STATE_TONE[desktopState]}>{t(APP_COPY.surfaces.desktop)}: {stateLabel(desktopState, locale)}</Status>
            <Status tone={STATE_TONE[hostedState]}>{t(APP_COPY.surfaces.browser)}: {stateLabel(hostedState, locale)}</Status>
          </p>
        }
        actions={
          <>
            <Link href={`/${locale}/install`} className="btn btn-primary btn-lg">
              {t(APP_COPY.actions.install)}
            </Link>
            <a href={APP_COPY.guideUrl} className="btn btn-secondary btn-lg">
              {t(APP_COPY.actions.guide)}
              <Icon name="external" className="icon" />
            </a>
            <Link href={`/${locale}/docs/account`} className="btn btn-ghost btn-lg">
              {t(APP_COPY.actions.signIn)}
            </Link>
          </>
        }
      />

      <div className="page-body">
        <Section id="app-shots" title={t(APP_COPY.shotsHeading)} scope={t(APP_COPY.shotsScope)}>
          <div className="app-shots">
            {APP_COPY.shots.map((shot) => (
              <figure className="figure" key={shot.file}>
                <div className="figure-frame">
                  <Image
                    src={`/app/${shot.file}.webp`}
                    alt={t(shot.alt)}
                    width={shot.width}
                    height={shot.height}
                    sizes="(max-width: 900px) 100vw, 1100px"
                    unoptimized
                  />
                </div>
                <figcaption className="figure-caption">{t(shot.caption)}</figcaption>
              </figure>
            ))}
          </div>
        </Section>

        <Section id="app-account" title={t(APP_COPY.accountHeading)} scope={t(APP_COPY.accountScope)}>
          <div className="grid-2">
            <div className="tile">
              <h3>{t(APP_COPY.withoutTitle)}</h3>
              <dl className="ruled-list">
                {APP_COPY.without.map((row) => (
                  <div key={row.title.en}>
                    <dt>{t(row.title)}</dt>
                    <dd>{t(row.body)}</dd>
                  </div>
                ))}
              </dl>
            </div>
            <div className="tile">
              <h3>{t(APP_COPY.withTitle)}</h3>
              <dl className="ruled-list">
                {APP_COPY.with.map((row) => (
                  <div key={row.title.en}>
                    <dt>{t(row.title)}</dt>
                    <dd>{t(row.body)}</dd>
                  </div>
                ))}
              </dl>
            </div>
          </div>
          <p className="app-note">
            {t(APP_COPY.signInNote)}{" "}
            <Link href={`/${locale}/docs/account`} className="link">
              {t(APP_COPY.actions.signIn)}
            </Link>
          </p>
        </Section>

        <Section id="app-runs" title={t(APP_COPY.runsHeading)}>
          <div className="data-table-wrap">
            <table className="data-table app-table">
              <thead>
                <tr>
                  <th scope="col">{t(APP_COPY.runsSurface)}</th>
                  <th scope="col">{t(APP_COPY.runsState)}</th>
                  <th scope="col">{t(APP_COPY.runsDetail)}</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row) => {
                  const view = describeRow(row, locale);
                  return (
                  <tr key={row.id}>
                    <th scope="row">{t(row.surface)}</th>
                    <td>
                      <Status tone={view.tone}>{view.status}</Status>
                    </td>
                    <td>
                      {view.detail}
                      {view.download ? <DesktopDownloadBlock download={view.download} locale={locale} /> : null}
                      {view.link && row.href && row.linkLabel ? (
                        <>
                          {" "}
                          <Link href={`/${locale}${row.href}`} className="link">
                            {t(row.linkLabel)}
                          </Link>
                        </>
                      ) : null}
                    </td>
                  </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </Section>

        <section className="page-section" aria-labelledby="app-start">
          <div className="callout">
            <h2 className="callout-title" id="app-start">{t(APP_COPY.closingTitle)}</h2>
            <p>{t(APP_COPY.closingBody)}</p>
            <div className="actions">
              <Link href={`/${locale}/install`} className="btn btn-primary">{t(APP_COPY.actions.install)}</Link>
              <a href={APP_COPY.guideUrl} className="btn btn-secondary">
                {t(APP_COPY.actions.guide)}
                <Icon name="external" className="icon" />
              </a>
            </div>
          </div>
        </section>
      </div>
    </>
  );
}
