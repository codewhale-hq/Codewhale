import Link from "next/link";
import { Icon, type IconName } from "@/components/icon";
import { PageHeader } from "@/components/page-header";
import { FeedCard } from "@/components/feed-card";
import { FeedRetry } from "@/components/feed-retry";
import { EmptyState, ErrorState, UnavailableState } from "@/components/surface-state";
import { loadFeed, type FeedLoadStatus } from "@/lib/github";
import { getEnv } from "@/lib/kv";
import { fill, getFeed, getStates, splitToken } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";
import type { FeedItem } from "@/lib/types";

export const revalidate = 600;

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getFeed(locale);
  return buildPageMetadata({
    path: "/feed",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function FeedPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const han = locale === "zh";
  const t = getFeed(locale);

  const env = await getEnv();
  let feed: FeedItem[] = [];
  // Four honest answers for an empty column: GitHub answered and had nothing
  // (`ok` → empty), GitHub was not asked or refused (`skipped` / `unavailable`
  // → not loaded, retry), or the fetch itself threw (`failed` → error, retry).
  // Each column answers for itself: one refused endpoint must not tell the
  // other column that its source did not answer.
  let issuesStatus: FeedLoadStatus | "failed" = "ok";
  let pullsStatus: FeedLoadStatus | "failed" = "ok";
  try {
    const load = await loadFeed(env.GITHUB_TOKEN, 50);
    feed = load.items;
    issuesStatus = load.issuesStatus;
    pullsStatus = load.pullsStatus;
  } catch (e) {
    issuesStatus = "failed";
    pullsStatus = "failed";
    console.error("feed fetch failed", e);
  }

  const issues = feed.filter((f) => f.kind === "issue");
  const pulls = feed.filter((f) => f.kind === "pull");
  const states = getStates(locale);
  // FeedRetry first busts this route's ISR entry, because a bare server
  // re-render would serve the same cached `skipped`/`unavailable` record for
  // up to ten minutes.
  const retry = <FeedRetry label={states.retry} />;
  const columnState = (status: FeedLoadStatus | "failed") =>
    status === "failed" ? (
      <ErrorState locale={locale} compact action={retry} />
    ) : status === "ok" ? (
      <EmptyState locale={locale} compact />
    ) : (
      <UnavailableState locale={locale} compact action={retry} />
    );

  // The repository link is typeset between the lede's halves, so a locale
  // may place the {repo} token anywhere.
  const ledeParts = splitToken(t.lede, "repo");
  const lede = (
    <>
      {ledeParts[0]}
      <Link href="https://github.com/codewhale-hq/CodeWhale" className="link">codewhale-hq/CodeWhale</Link>
      {ledeParts[1]}
    </>
  );
  const actionLinks: { href: string; icon: IconName; label: string }[] = [
    { href: "https://github.com/codewhale-hq/CodeWhale/issues/new/choose", icon: "alert", label: t.openIssue },
    { href: "https://github.com/codewhale-hq/CodeWhale/compare", icon: "git-pull-request", label: t.openPull },
    { href: "https://github.com/codewhale-hq/CodeWhale/discussions/categories/q-a", icon: "message", label: t.startDiscussion },
  ];
  const nothingLoaded = feed.length === 0 && issuesStatus !== "ok" && pullsStatus !== "ok";
  const columns = [
    { id: "feed-pulls", title: t.pulls, items: pulls, status: pullsStatus },
    { id: "feed-issues", title: t.issues, items: issues, status: issuesStatus },
  ];

  return (
    <>
      <PageHeader
        seal={han ? "动" : undefined}
        title={t.title}
        titleAside={han ? t.titleAside : undefined}
        titleAsideLang={han ? t.titleAsideLang : undefined}
        lede={lede}
        pose="browse"
      />
      <div className="page-body">
        {nothingLoaded ? (
          <div className="feed-empty">{columnState(issuesStatus === "failed" || pullsStatus === "failed" ? "failed" : issuesStatus)}</div>
        ) : (
        <div className="grid-2 feed-columns">
          {columns.map((column) => (
            <section key={column.id} className="feed-column" aria-labelledby={column.id}>
              <div className="feed-column-head">
                <h2 id={column.id}>{column.title}</h2>
                <span className="page-meta tabular">{fill(t.shownCount, { count: column.items.length })}</span>
              </div>
              {column.items.length > 0 ? (
                <ul className="feed-items" role="list">
                  {column.items.map((item) => (
                    <li key={item.url}><FeedCard item={item} /></li>
                  ))}
                </ul>
              ) : (
                <div className="feed-empty">{columnState(column.status)}</div>
              )}
            </section>
          ))}
        </div>
        )}

        <section className="page-section">
          <ul className="grid-3" role="list">
            {actionLinks.map((link) => (
              <li key={link.href}>
                <Link href={link.href} className="dir-row feed-action">
                  <span className="dir-mark" aria-hidden="true"><Icon name={link.icon} /></span>
                  <span className="dir-text"><span className="dir-title">{link.label}</span></span>
                  <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      </div>
    </>
  );
}
