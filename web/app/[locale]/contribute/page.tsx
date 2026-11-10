import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { getContribute, pickText } from "@/lib/i18n/dictionaries";
import { REPO_ISSUES_URL, REPO_URL } from "@/lib/i18n/links";
import { buildPageMetadata } from "@/lib/page-meta";
import { getMerchCopy } from "@/lib/content/contributor-merch";
import { SKILL_CONTRIBUTION } from "@/lib/content/contributing";
import { CONTRIBUTION_ROUTES, PROJECT_COPY, STANDING_PROJECTS } from "@/lib/content/project";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getContribute(locale);
  return buildPageMetadata({
    path: "/contribute",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function ContributePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getContribute(locale);
  const merch = getMerchCopy(locale);
  const standing = [...STANDING_PROJECTS, SKILL_CONTRIBUTION];

  return (
    <>
      <PageHeader
        kicker={t.kicker}
        title={t.title}
        lede={t.lede}
        pose="write"
        actions={
          <>
            <Link href={`${REPO_ISSUES_URL}/new/choose`} className="btn btn-primary btn-lg">
              {t.fileIssue}
              <Icon name="external" className="icon" />
            </Link>
            <Link href={`${REPO_URL}/blob/main/CONTRIBUTING.md`} className="btn btn-secondary btn-lg">
              {t.fullGuide}
              <Icon name="external" className="icon" />
            </Link>
          </>
        }
      />

      <div className="page-body">
        <Section
          id="contribute-routes"
          title={pickText(PROJECT_COPY.routesHeading, locale)}
          scope={pickText(PROJECT_COPY.routesScope, locale)}
        >
          <ul className="dir-list dir-list-card route-list" role="list">
            {CONTRIBUTION_ROUTES.map((route) => (
              <li key={route.have.en}>
                <a href={route.href} className="dir-row">
                  <span className="dir-text">
                    <span className="dir-title">{pickText(route.have, locale)}</span>
                  </span>
                  <span className="route-place">{pickText(route.place, locale)}</span>
                  <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                </a>
              </li>
            ))}
          </ul>
        </Section>

        <Section
          id="contribute-features"
          layout="split"
          label={pickText(PROJECT_COPY.featureLabel, locale)}
          title={pickText(PROJECT_COPY.featureHeading, locale)}
          scope={pickText(PROJECT_COPY.featureScope, locale)}
        >
          <ol className="goal-list" role="list">
            {PROJECT_COPY.featureSteps.map((step, index) => (
              <li key={step.title.en} className="goal">
                <span className="goal-number tabular" aria-hidden="true">{String(index + 1).padStart(2, "0")}</span>
                <div className="goal-text">
                  <h3>{pickText(step.title, locale)}</h3>
                  <p>{pickText(step.body, locale)}</p>
                </div>
              </li>
            ))}
          </ol>
        </Section>

        <Section
          id="contribute-standing"
          title={pickText(PROJECT_COPY.standingHeading, locale)}
          scope={pickText(PROJECT_COPY.standingScope, locale)}
        >
          <div className="grid-3">
            {standing.map((project) => (
              <a key={project.href} href={project.href} className="tile">
                <h3>{pickText(project.title, locale)}</h3>
                <p>{pickText(project.body, locale)}</p>
              </a>
            ))}
          </div>
        </Section>

        <Section
          id="contribute-expect"
          layout="split"
          label={pickText(PROJECT_COPY.expectLabel, locale)}
          title={pickText(PROJECT_COPY.expectHeading, locale)}
          link={<Link href={`/${locale}/roadmap`} className="btn btn-secondary">{pickText(PROJECT_COPY.goalsLink, locale)}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
        >
          <dl className="ruled-list ruled-list-stacked">
            {PROJECT_COPY.expect.map((item) => (
              <div key={item.title.en}>
                <dt>{pickText(item.title, locale)}</dt>
                <dd>{pickText(item.body, locale)}</dd>
              </div>
            ))}
          </dl>
        </Section>

        <Section id="contributor-merch" title={merch.title} scope={merch.lede}>
          <Link href={`/${locale}/merch`} className="btn btn-secondary">{merch.cta}</Link>
        </Section>
      </div>
    </>
  );
}
