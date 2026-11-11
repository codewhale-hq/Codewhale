import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader } from "@/components/page-header";
import { Status } from "@/components/status-badge";
import { EmptyState } from "@/components/surface-state";
import { ReleaseUpdates } from "@/components/release-updates";
import { getReleaseUpdates } from "@/lib/release-updates";
import { CHANGELOG, type ChangelogRelease } from "@/lib/changelog.generated";
import { changelogAnchor } from "@/scripts/changelog-lib.mjs";
import { getFacts } from "@/lib/facts";
import { compareVersions } from "@/lib/features";
import { fill, getChangelog, getChrome } from "@/lib/i18n/dictionaries";
import { REPO_RELEASES_URL, REPO_URL } from "@/lib/i18n/links";
import { buildPageMetadata } from "@/lib/page-meta";
import { bodyDateLocale } from "@/lib/i18n/fallback";
import { FallbackRegion } from "@/components/fallback-region";

// Published releases resolve at request time; the notes are build-time
// from CHANGELOG.md and link to the complete published history.
export const dynamic = "force-dynamic";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getChangelog(locale);
  return buildPageMetadata({
    path: "/changelog",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

/** Link labels carry a trailing ↗ in the dictionaries; the icon draws it. */
function ext(label: string) {
  return (
    <>
      {label.replace(/\s*↗\s*$/u, "")}
      <Icon name="external" className="icon" />
    </>
  );
}

function formatDate(iso: string, dateLocale: string): string {
  return new Date(iso).toLocaleDateString(dateLocale, {
    year: "numeric",
    month: "short",
    day: "numeric",
    // Dates come from ISO timestamps and YYYY-MM-DD headings; render them
    // as the UTC day they name, not the server's local day.
    timeZone: "UTC",
  });
}

export default async function ChangelogPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getChangelog(locale);
  const chrome = getChrome(locale);
  const facts = await getFacts();
  const updates = await getReleaseUpdates(facts.latestPublishedRelease);
  const published = updates.terminal.release;
  const candidate = facts.version;
  const candidateIsPublished = published !== null && candidate !== null && published.version === candidate;

  return (
    <FallbackRegion path="/changelog" locale={locale}>
      <PageHeader kicker={t.kicker} title={t.title} lede={t.lead} pose="write">
        <dl className="def-rows changelog-facts">
          <div className="def-row" data-candidate={candidateIsPublished ? "published" : "unreleased"}>
            <dt>{t.candidateLabel}</dt>
            <dd>
              {candidate && (
                <Status tone={candidateIsPublished ? "ready" : "idle"}>
                  {candidateIsPublished
                    ? fill(t.candidateMatches, { version: `v${candidate}` })
                    : fill(t.candidateValue, { version: `v${candidate}` })}
                </Status>
              )}
              <a href={`${REPO_URL}/blob/main/CHANGELOG.md`} target="_blank" rel="noreferrer" className="link">
                {ext(t.fullNotes)}
              </a>
            </dd>
          </div>
        </dl>
      </PageHeader>

      <div className="page-body">
        <ReleaseUpdates key={`${locale}:${updates.fetchedAt}`} locale={locale} initial={updates} showAllLink={false} />
        <div className="page-body-narrow changelog-list">
          {CHANGELOG.length === 0 ? (
            <EmptyState
              locale={locale}
              title={t.emptyTitle}
              body={t.emptyBody}
              action={
                <a className="btn btn-secondary" href={REPO_RELEASES_URL}>
                  {ext(t.releasesLink)}
                </a>
              }
            />
          ) : (
            CHANGELOG.map((release) => (
              <Release
                key={release.version}
                release={release}
                locale={locale}
                candidate={!release.unreleased && published !== null && (compareVersions(release.version, published.version) ?? 0) > 0}
                publishedUrl={
                  published && published.version === release.version ? published.url : null
                }
              />
            ))
          )}
          <p className="status-line">
            <Link href={`/${locale}/docs`} className="link">
              {chrome.footerDocs}
            </Link>
            <a href={REPO_RELEASES_URL} target="_blank" rel="noreferrer" className="link">
              {ext(t.releasesLink)}
            </a>
          </p>
        </div>
      </div>
    </FallbackRegion>
  );
}

function Release({
  release,
  locale,
  candidate,
  publishedUrl,
}: {
  release: ChangelogRelease;
  locale: string;
  candidate: boolean;
  publishedUrl: string | null;
}) {
  const t = getChangelog(locale);
  const heading = release.unreleased ? t.unreleasedHeading : `v${release.version}`;
  const anchor = release.unreleased ? "unreleased" : `v${release.version}`;
  // Every entry here is clipped or capped for the web; this is the in-page
  // path to the unabridged notes for exactly this version.
  const notesUrl = `${REPO_URL}/blob/main/CHANGELOG.md#${changelogAnchor(release)}`;
  const versionUrl = release.unreleased ? null : `/${locale}/changelog/${release.version}`;
  const notesLabel = fill(t.releaseNotesLink, { version: heading });

  return (
    <article id={anchor} className="changelog-release scroll-mt-32">
      <div className="changelog-release-head">
        {/* Each release is a receipt: the shell marks it. */}
        <h2>
          <Icon name="shell" />
          {heading}
        </h2>
        <div className="changelog-links">
          {release.date && <span className="tabular">{formatDate(release.date, bodyDateLocale("/changelog", locale))}</span>}
          {publishedUrl && (
            <a href={publishedUrl} target="_blank" rel="noreferrer">
              {ext(t.releasePageLink)}
            </a>
          )}
          {release.compareUrl && !release.unreleased && !candidate && (
            <a href={release.compareUrl} target="_blank" rel="noreferrer">
              {ext(t.compareLink)}
            </a>
          )}
          {versionUrl ? (
            <Link href={versionUrl}>{notesLabel.replace(/\s*↗\s*$/u, "")}</Link>
          ) : (
            <a href={notesUrl} target="_blank" rel="noreferrer">
              {ext(notesLabel)}
            </a>
          )}
        </div>
      </div>
      {(release.unreleased || candidate) && <p className="changelog-unreleased-note">{t.unreleasedNote}</p>}
      <div className="changelog-sections">
        {release.sections.map((section, i) => (
          <section key={`${section.heading}-${i}`}>
            <h3>
              {section.heading}
              {section.itemCount > section.items.length &&
                (versionUrl ? (
                  <Link href={versionUrl}>
                    {fill(t.moreEntries, { shown: section.items.length, total: section.itemCount })}
                  </Link>
                ) : (
                  <a href={notesUrl} target="_blank" rel="noreferrer">
                    {fill(t.moreEntries, { shown: section.items.length, total: section.itemCount })}
                  </a>
                ))}
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
  );
}
