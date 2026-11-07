"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { Section } from "@/components/page-header";
import { Icon } from "@/components/icon";
import { readBoundedBody } from "@/lib/bounded-body";
import { COMPUTER_USE_REPO } from "@/lib/computer-use-release";
import { RELEASE_UPDATES_COPY } from "@/lib/content/release-updates";
import { getChrome, pickText } from "@/lib/i18n/dictionaries";
import { REPO_RELEASES_URL } from "@/lib/i18n/links";
import type { ReleaseUpdatesSnapshot } from "@/lib/release-updates";

const INTERVAL = 5 * 60 * 1000;
const SOURCES = { terminal: REPO_RELEASES_URL, computerUse: `${COMPUTER_USE_REPO}/releases` };
const CHANNELS = ["terminal", "computerUse"] as const;

function validSnapshot(value: unknown): value is ReleaseUpdatesSnapshot {
  if (!value || typeof value !== "object") return false;
  const snapshot = value as Record<string, unknown>;
  if (snapshot.fetchedAt !== null && (typeof snapshot.fetchedAt !== "string" || !Number.isFinite(Date.parse(snapshot.fetchedAt)))) return false;
  return CHANNELS.every((channel) => {
    const item = snapshot[channel];
    if (!item || typeof item !== "object") return false;
    const state = item as Record<string, unknown>;
    if (!["current", "cached", "unavailable"].includes(String(state.status))) return false;
    if (state.release === null) return state.status === "unavailable";
    if (state.status === "unavailable") return false;
    if (!state.release || typeof state.release !== "object") return false;
    const release = state.release as Record<string, unknown>;
    return typeof release.tag === "string" && /^v\d+\.\d+\.\d+$/.test(release.tag)
      && release.version === release.tag.slice(1)
      && release.url === `${SOURCES[channel]}/tag/${release.tag}`
      && typeof release.publishedAt === "string" && Number.isFinite(Date.parse(release.publishedAt));
  });
}

export function ReleaseUpdates({ locale, initial, showAllLink = true }: { locale: string; initial: ReleaseUpdatesSnapshot; showAllLink?: boolean }) {
  const [snapshot, setSnapshot] = useState(initial);
  const [failed, setFailed] = useState(false);
  const text = (key: keyof typeof RELEASE_UPDATES_COPY) => pickText(RELEASE_UPDATES_COPY[key], locale);
  const dateLocale = getChrome(locale).dateLocale;

  useEffect(() => {
    let active = true;
    let pending: AbortController | null = null;
    let lastAttempt = CHANNELS.every((channel) => initial[channel].status === "current") ? Math.min(Date.now(), Date.parse(initial.fetchedAt ?? "") || 0) : 0;
    const refresh = async () => {
      if (document.visibilityState !== "visible" || pending || Date.now() - lastAttempt < INTERVAL) return;
      lastAttempt = Date.now();
      pending = new AbortController();
      const timeout = window.setTimeout(() => pending?.abort(), 15_000);
      try {
        const response = await fetch("/api/releases", { cache: "no-store", credentials: "omit", signal: pending.signal });
        if (!response.ok) throw new Error("Release check unavailable");
        const next: unknown = JSON.parse(new TextDecoder().decode(await readBoundedBody(response, 32 * 1024)));
        if (!validSnapshot(next)) throw new Error("Invalid release response");
        if (active) {
          setSnapshot((previous) => {
            const resolved = { ...next };
            for (const channel of CHANNELS) {
              const before = previous[channel].release;
              const after = next[channel].release;
              if (before && next[channel].status !== "current" && (!after || Date.parse(before.publishedAt) > Date.parse(after.publishedAt))) {
                resolved[channel] = { release: before, status: "cached" };
              }
            }
            return resolved;
          });
          setFailed(false);
        }
      } catch {
        if (active) setFailed(true);
      } finally {
        window.clearTimeout(timeout);
        pending = null;
      }
    };
    void refresh();
    const interval = window.setInterval(() => void refresh(), INTERVAL);
    const visible = () => void refresh();
    document.addEventListener("visibilitychange", visible);
    window.addEventListener("focus", visible);
    return () => {
      active = false;
      window.clearInterval(interval);
      pending?.abort();
      document.removeEventListener("visibilitychange", visible);
      window.removeEventListener("focus", visible);
    };
  }, [initial]);

  return (
    <Section
      id="release-updates"
      title={text("title")}
      className="home-section release-updates"
      scope={text("source")}
      link={showAllLink ? <Link href={`/${locale}/changelog`} className="section-link">{text("all")}<Icon name="arrow-right" className="icon icon-flip" /></Link> : undefined}
    >
      <div className="release-cards" data-checked-at={snapshot.fetchedAt ?? undefined}>
        {CHANNELS.map((channel) => {
          const { release, status } = snapshot[channel];
          return (
            <article className="release-card" key={channel} aria-label={text(channel)}>
              <h3>{text(channel)}</h3>
              <div className="release-version" aria-live="polite">
                {release ? <a href={release.url}>{release.tag}</a> : <a href={SOURCES[channel]}>{text("unavailable")}</a>}
                {release?.publishedAt ? <time dateTime={release.publishedAt}>{new Date(release.publishedAt).toLocaleDateString(dateLocale, { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" })}</time> : null}
                {release && (status === "cached" || failed) ? <span className="release-cached">{text("cached")}</span> : null}
              </div>
              <div className="release-links">
                <a href={release?.url ?? SOURCES[channel]} className="section-link">{text("notes")}<Icon name="external" className="icon" /></a>
                <a href={`${SOURCES[channel]}.atom`} className="section-link" aria-label={`${text(channel)} · ${text("rss")}`}>{text("rss")}</a>
              </div>
            </article>
          );
        })}
      </div>
      {failed ? <p className="release-update-error" role="status">{text("refreshFailed")}</p> : null}
    </Section>
  );
}
