import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { GitHubStar, FaceWall, MergedList } from "@/components/project-pulse";
import { getFacts } from "@/lib/facts";
import { fetchLabelled, type OpenIssue } from "@/lib/github";
import { getEnv } from "@/lib/kv";
import { loadProjectPulse } from "@/lib/project-pulse";
import { getContribute, pickText } from "@/lib/i18n/dictionaries";
import { REPO_ISSUES_URL, REPO_URL } from "@/lib/i18n/links";
import { buildPageMetadata } from "@/lib/page-meta";
import { getMerchCopy } from "@/lib/content/contributor-merch";
import { SKILL_CONTRIBUTION } from "@/lib/content/contributing";
import { CONTRIBUTION_ROUTES, PROJECT_COPY, STANDING_PROJECTS } from "@/lib/content/project";

export const revalidate = 600;

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
  const facts = await getFacts();
  const pulse = await loadProjectPulse(8, 96);
  let openNow: OpenIssue[] = [];
  try {
    openNow = await fetchLabelled("help wanted", (await getEnv()).GITHUB_TOKEN, 6);
  } catch {
    openNow = [];
  }

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
            <GitHubStar stars={pulse.stats?.stars} locale={locale} />
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

        {openNow.length > 0 ? (
          <Section
            id="contribute-open"
            title={pickText(PROJECT_COPY.openNowHeading, locale)}
            scope={pickText(PROJECT_COPY.openNowScope, locale)}
            link={<Link href={`${REPO_ISSUES_URL}?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22`} className="section-link">GitHub<Icon name="external" className="icon" /></Link>}
          >
            <ul className="dir-list dir-list-card" role="list">
              {openNow.map((issue) => (
                <li key={issue.number}>
                  <a href={issue.url} className="dir-row" data-funnel="help_wanted_exit">
                    <span className="dir-mark" aria-hidden="true"><Icon name="sonar" /></span>
                    <span className="dir-text">
                      <span className="dir-title">{issue.title}</span>
                      <span className="dir-purpose">{issue.labels.slice(0, 3).join(" · ")}</span>
                    </span>
                    <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                  </a>
                </li>
              ))}
            </ul>
          </Section>
        ) : null}

        {pulse.merged.length > 0 || pulse.faces.length > 0 ? (
          <Section
            id="contribute-merged"
            layout="split"
            label={pickText(PROJECT_COPY.liveLabel, locale)}
            title={pickText(PROJECT_COPY.mergedPageHeading, locale)}
            scope={pickText(PROJECT_COPY.mergedPageScope, locale)}
            link={<Link href={`/${locale}/roadmap`} className="btn btn-secondary">{pickText(PROJECT_COPY.goalsLink, locale)}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
          >
            <MergedList items={pulse.merged} locale={locale} />
          </Section>
        ) : null}

        <FaceWall faces={pulse.faces} total={facts.contributorCount} locale={locale} />

        <Section id="contributor-merch" title={merch.title} scope={merch.lede}>
          <Link href={`/${locale}/merch`} className="btn btn-secondary">{merch.cta}</Link>
        </Section>
      </div>
    </>
  );
}
