import Link from "next/link";
import { notFound, permanentRedirect } from "next/navigation";
import { Icon } from "@/components/icon";
import { PageHeader } from "@/components/page-header";
import { RELEASE_NOTES } from "@/lib/release-notes.generated";
import { changelogAnchor } from "@/scripts/changelog-lib.mjs";
import { fill, getChangelog } from "@/lib/i18n/dictionaries";
import { REPO_URL } from "@/lib/i18n/links";
import { buildPageMetadata } from "@/lib/page-meta";
import { bodyDateLocale } from "@/lib/i18n/fallback";
import { FallbackRegion } from "@/components/fallback-region";

type PageParams = Promise<{ locale: string; version: string }>;

export function generateStaticParams() {
  return [];
}

function findRelease(version: string) {
  return RELEASE_NOTES.find((release) => release.version === version);
}

export async function generateMetadata({ params }: { params: PageParams }) {
  const { locale, version } = await params;
  const release = findRelease(version);
  if (!release) notFound();
  const t = getChangelog(locale);
  return buildPageMetadata({
    path: `/changelog/${release.version}`,
    locale,
    title: fill(t.versionMetaTitle, { version: release.version }),
    description: fill(t.versionMetaDescription, { version: release.version }),
  });
}

export default async function ReleaseNotesPage({ params }: { params: PageParams }) {
  const { locale, version } = await params;
  const release = findRelease(version);
  if (!release) {
    const bare = version.replace(/^v/i, "");
    if (bare !== version && findRelease(bare)) permanentRedirect(`/${locale}/changelog/${bare}`);
    notFound();
  }
  const t = getChangelog(locale);
  const date = release.date
    ? new Date(release.date).toLocaleDateString(bodyDateLocale("/changelog", locale), {
        year: "numeric",
        month: "long",
        day: "numeric",
        timeZone: "UTC",
      })
    : null;
  const notesUrl = `${REPO_URL}/blob/main/CHANGELOG.md#${changelogAnchor(release)}`;

  return (
    <FallbackRegion path="/changelog" locale={locale}>
      <PageHeader
        kicker={t.versionKicker}
        title={`Codewhale ${release.version}`}
        lede={date ? fill(t.versionLead, { date }) : t.versionLeadUndated}
        pose="write"
        actions={
          <>
            <Link href={`/${locale}/changelog`} className="btn btn-secondary">
              {t.allReleasesLink}
            </Link>
            <a href={notesUrl} target="_blank" rel="noreferrer" className="section-link">
              {t.fullNotes.replace(/\s*↗\s*$/u, "")}
              <Icon name="external" className="icon" />
            </a>
          </>
        }
      />

      <div className="page-body">
        <div className="page-body-narrow changelog-list">
          <article id={`v${release.version}`} className="changelog-release">
            <div className="changelog-sections">
              {release.sections.map((section, i) => (
                <section key={`${section.heading}-${i}`}>
                  <h3>
                    {section.heading}
                    <span className="tabular">{section.itemCount}</span>
                  </h3>
                  <ul>
                    {section.items.map((item, j) => (
                      <li key={j}>{item}</li>
                    ))}
                  </ul>
                </section>
              ))}
            </div>
          </article>
        </div>
      </div>
    </FallbackRegion>
  );
}
