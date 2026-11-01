import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { Status } from "@/components/status-badge";
import { getEnterprise } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const copy = getEnterprise(locale);
  return buildPageMetadata({ path: "/enterprise", locale, title: copy.metaTitle, description: copy.metaDescription });
}

export default async function EnterprisePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getEnterprise(locale);
  const mailto = `mailto:${t.partnerEmail}?subject=${encodeURIComponent("Codewhale Enterprise design partner")}`;

  return (
    <>
      <PageHeader
        kicker={t.kicker}
        title={t.title}
        lede={t.lead}
        pose="think"
        meta={<Status tone="attention">{t.status}</Status>}
        actions={
          <div className="actions">
            <a className="btn btn-primary btn-lg" href={mailto}>{t.partnerCta}</a>
            <Link className="btn btn-ghost" href={`/${locale}/install`}>{t.secondaryCta}</Link>
          </div>
        }
      />

      <div className="page-body">
        <Section id="enterprise-loop" title={t.loopTitle} scope={t.loopScope}>
          <ol className="steps">
            {t.loop.map((step, index) => (
              <li key={step.title}>
                <span className="gs-step-index" aria-hidden="true">{index + 1}</span>
                <h3>{step.title}</h3>
                <p>{step.body}</p>
              </li>
            ))}
          </ol>
        </Section>

        <Section id="enterprise-status" title={t.statusTitle} scope={t.statusScope}>
          <div className="roadmap-tracks">
            {[
              { title: t.todayTitle, tone: "ready" as const, items: t.today },
              { title: t.plannedTitle, tone: "idle" as const, items: t.planned },
            ].map((track) => (
              <section key={track.title} className="roadmap-track" aria-label={track.title}>
                <div className="roadmap-track-head">
                  <h3><Status tone={track.tone}>{track.title}</Status></h3>
                </div>
                <ul className="group-card" role="list">
                  {track.items.map((item) => (
                    <li key={item.title} className="group-row roadmap-item">
                      <p className="roadmap-item-title">{item.title}</p>
                      <p className="roadmap-item-note">{item.body}</p>
                    </li>
                  ))}
                </ul>
              </section>
            ))}
          </div>
        </Section>

        <section className="page-section" aria-label={t.controlTitle}>
          <div className="grid-2">
            <div className="tile">
              <span className="tile-icon" aria-hidden="true"><Icon name="folder" /></span>
              <h2 className="section-title">{t.dataTitle}</h2>
              <p>{t.dataBody}</p>
            </div>
            <div className="tile">
              <span className="tile-icon" aria-hidden="true"><Icon name="shield" /></span>
              <h2 className="section-title">{t.engineTitle}</h2>
              <p>{t.engineBody}</p>
            </div>
          </div>
        </section>

        <section className="page-section" aria-labelledby="enterprise-partners">
          <div className="callout">
            <p className="callout-title" id="enterprise-partners">
              <Icon name="users" className="icon" />
              {t.partnerTitle}
            </p>
            <p>{t.partnerBody}</p>
            <div className="actions">
              <a className="btn btn-secondary" href={mailto}>{t.partnerEmail}</a>
            </div>
          </div>
        </section>
      </div>
    </>
  );
}
