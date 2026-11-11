import { Icon } from "./icon";
import { LiveMergedList, LivePulseStats, LiveStarCount, type PulseRow } from "./github-live";
import { GITHUB_STAR_COPY } from "@/lib/content/github-star";
import { PROJECT_COPY } from "@/lib/content/project";
import { formatStars, relativeAge, type Face } from "@/lib/github";
import { fill, getChrome, pickText } from "@/lib/i18n/dictionaries";
import { REPO_URL } from "@/lib/i18n/links";
import type { FeedItem, RepoStats } from "@/lib/types";

const avatar = (url: string, size: number) => `${url}${url.includes("?") ? "&" : "?"}s=${size}`;

function age(iso: string, locale: string): string {
  const { value, unit } = relativeAge(iso);
  return new Intl.RelativeTimeFormat(getChrome(locale).dateLocale, { numeric: "auto" }).format(value, unit);
}

export function PulseStats({ stats, contributors, locale }: { stats: RepoStats | null; contributors: number; locale: string }) {
  const rows: PulseRow[] = [
    { key: "stars", label: pickText(PROJECT_COPY.statStars, locale), href: REPO_URL, value: stats?.stars != null ? formatStars(stats.stars) : undefined },
    { key: "contributors", label: pickText(PROJECT_COPY.statContributors, locale), href: `${REPO_URL}/blob/main/docs/CONTRIBUTORS.md`, value: String(contributors) },
    { key: "pulls", label: pickText(PROJECT_COPY.statPulls, locale), href: `${REPO_URL}/pulls`, value: stats?.openPulls != null ? String(stats.openPulls) : undefined },
    { key: "issues", label: pickText(PROJECT_COPY.statIssues, locale), href: `${REPO_URL}/issues`, value: stats?.openIssues != null ? String(stats.openIssues) : undefined },
  ];
  return <LivePulseStats rows={rows} numberLocale={getChrome(locale).dateLocale} />;
}

export function MergedList({ items, locale, limit = 5 }: { items: FeedItem[]; locale: string; limit?: number }) {
  if (items.length === 0) {
    return (
      <LiveMergedList
        limit={limit}
        copy={{
          heading: pickText(PROJECT_COPY.mergedHeading, locale),
          mergedAge: pickText(PROJECT_COPY.mergedAge, locale),
          firstContribution: pickText(PROJECT_COPY.firstContribution, locale),
          dateLocale: getChrome(locale).dateLocale,
        }}
      />
    );
  }
  return (
    <ol className="merged-list" role="list">
      {items.map((item) => (
        <li key={item.number}>
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src={avatar(item.authorAvatar, 80)} alt="" width={40} height={40} loading="lazy" decoding="async" className="face" />
          <div className="merged-text">
            <a href={item.url} className="merged-title">{item.title}</a>
            <p className="merged-meta">
              <a href={`https://github.com/${item.author}`}>@{item.author}</a>
              <span>{fill(pickText(PROJECT_COPY.mergedAge, locale), { age: age(item.eventAt ?? item.updatedAt, locale) })}</span>
              {item.firstTimeContributor ? <span className="merged-first">{pickText(PROJECT_COPY.firstContribution, locale)}</span> : null}
            </p>
          </div>
        </li>
      ))}
    </ol>
  );
}

export function FaceWall({ faces, total, locale }: { faces: Face[]; total: number; locale: string }) {
  if (faces.length === 0) return null;
  return (
    <div className="face-wall">
      <ul role="list">
        {faces.map((face) => (
          <li key={face.login}>
            <a href={`https://github.com/${face.login}`} title={`@${face.login}`} aria-label={`@${face.login}`}>
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img src={avatar(face.avatar, 96)} alt="" width={44} height={44} loading="lazy" decoding="async" className="face" />
            </a>
          </li>
        ))}
      </ul>
      <a href={`${REPO_URL}/blob/main/docs/CONTRIBUTORS.md`} className="section-link">
        {fill(pickText(PROJECT_COPY.facesAll, locale), { count: total })}
        <Icon name="external" className="icon" />
      </a>
    </div>
  );
}

export function GitHubStar({ stars, locale }: { stars?: number; locale: string }) {
  const label = pickText(GITHUB_STAR_COPY, locale);
  return (
    <a href={REPO_URL} className="btn btn-secondary github-star" data-funnel="star_exit">
      <span aria-hidden="true">☆</span>
      <span>{label}</span>
      <LiveStarCount initial={stars} numberLocale={getChrome(locale).dateLocale} />
    </a>
  );
}
