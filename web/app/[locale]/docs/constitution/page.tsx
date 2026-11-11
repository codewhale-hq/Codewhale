import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { Status } from "@/components/status-badge";
import { CONSTITUTION_LAYERS } from "@/lib/content/constitution";
import { ThinkingTrace } from "@/components/thinking-trace";
import { getConstitution, pickText } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getConstitution(locale);
  return buildPageMetadata({
    path: "/docs/constitution",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function ConstitutionPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const han = locale === "zh";
  const t = getConstitution(locale);

  return (
    <>
      <PageHeader
        titleId="constitution-overview"
        seal={han ? "法" : undefined}
        kicker={t.kicker}
        title={t.title}
        titleAside={han ? t.titleAside : undefined}
        titleAsideLang={han ? t.titleAsideLang : undefined}
        lede={t.lede}
        pose="read"
      >
        <div className="callout">
          <p className="callout-title">
            <Status tone="accent">{t.since}</Status>
          </p>
          <p>{t.sinceBody}</p>
        </div>
      </PageHeader>

      <div className="page-body">
        <Section
          id="constitution-rank"
          seal={han ? "序" : undefined}
          title={t.rankTitle}
          scope={t.rankScope}
        >
          <ol className="steps">
            {CONSTITUTION_LAYERS.map((layer, index) => (
              <li key={layer.n}>
                <span className="gs-step-index" aria-hidden="true">{index + 1}</span>
                <h3>{pickText(layer.name, locale)}</h3>
                <p className="dir-meta">{pickText(layer.path, locale)}</p>
                <p>{pickText(layer.body, locale)}</p>
              </li>
            ))}
          </ol>
          <div className="callout mt-5">
            <p className="callout-title">
              <Icon name="shield" className="icon" />
              {t.boundaryTitle}
            </p>
            <p>{t.boundaryBody}</p>
          </div>
        </Section>

        <Section
          id="constitution-trace"
          seal={han ? "证" : undefined}
          title={t.traceTitle}
          scope={t.traceScope}
        >
          <ThinkingTrace locale={locale} />
        </Section>

        <section id="constitution-next" className="page-section">
          <div className="actions">
            <Link href={`/${locale}/install`} className="btn btn-primary btn-lg">
              {t.install}
            </Link>
            <Link
              href="https://github.com/codewhale-hq/CodeWhale/blob/main/docs/CONFIGURATION.md#constitution-project-instructions-and-repo-authority"
              className="btn btn-ghost btn-lg"
            >
              {t.configuration}
              <Icon name="external" className="icon" />
            </Link>
          </div>
        </section>
      </div>
    </>
  );
}
