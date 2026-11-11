import Link from "next/link";
import type { RepoFacts } from "@/lib/facts";
import { fill, getChrome, getDocsShell } from "@/lib/i18n/dictionaries";

export function ReleaseTruth({ locale, facts }: { locale: string; facts: RepoFacts }) {
  const t = getDocsShell(locale);
  const chrome = getChrome(locale);
  const published = facts.latestPublishedRelease;

  return (
    <div className="release-truth dotline" data-release-state={published ? "published" : "unknown"}>
      <span className="release-truth-label">{t.releaseLabel}</span>
      {published && (
        <a href={published.url} target="_blank" rel="noreferrer">
          {fill(t.releasePublished, {
            tag: published.tag,
            date: new Date(published.publishedAt).toLocaleDateString(chrome.dateLocale, {
              year: "numeric",
              month: "short",
              day: "numeric",
              timeZone: "UTC",
            }),
          })}
        </a>
      )}
      {published && <span>{fill(t.releaseDescribes, { tag: published.tag })}</span>}
      <Link href={`/${locale}/changelog`}>{t.releaseChangelog}</Link>
    </div>
  );
}
