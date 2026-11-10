import { Icon } from "./icon";
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
  const rows: { value: string; label: string; href: string }[] = [
    { value: String(contributors), label: pickText(PROJECT_COPY.statContributors, locale), href: `${REPO_URL}/blob/main/docs/CONTRIBUTORS.md` },
  ];
  if (stats) {
    rows.unshift({ value: formatStars(stats.stars), label: pickText(PROJECT_COPY.statStars, locale), href: REPO_URL });
    rows.push(
      { value: String(stats.openPulls), label: pickText(PROJECT_COPY.statPulls, locale), href: `${REPO_URL}/pulls` },
      { value: String(stats.openIssues), label: pickText(PROJECT_COPY.statIssues, locale), href: `${REPO_URL}/issues` },
    );
  }
  return (
    <dl className="pulse-stats">
      {rows.map((row) => (
        <div key={row.label}>
          <dt>{row.label}</dt>
          <dd><a href={row.href} className="tabular">{row.value}</a></dd>
        </div>
      ))}
    </dl>
  );
}

export function MergedList({ items, locale }: { items: FeedItem[]; locale: string }) {
  if (items.length === 0) return null;
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
  const count = stars === undefined ? null : new Intl.NumberFormat(getChrome(locale).dateLocale).format(stars);
  return (
    <a href={REPO_URL} className="btn btn-secondary github-star">
      <span aria-hidden="true">☆</span>
      <span>{label}</span>
      {count !== null && <span className="github-star-count tabular">{count}</span>}
    </a>
  );
}
