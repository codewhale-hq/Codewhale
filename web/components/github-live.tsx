"use client";

import { useEffect, useState } from "react";

const API = "https://api.github.com";
const REPO = "codewhale-hq/CodeWhale";
const CACHE_KEY = "codewhale:github-live:v1";
const CACHE_MS = 10 * 60 * 1000;

export interface LiveMerge {
  number: number;
  title: string;
  url: string;
  author: string;
  avatar: string;
  mergedAt: string;
  firstTime: boolean;
}

export interface LiveRepo {
  stars: number;
  openPulls: number;
  openIssues: number;
  merged: LiveMerge[];
}

let pending: Promise<LiveRepo | null> | null = null;

async function json(url: string): Promise<unknown> {
  const res = await fetch(url, { headers: { Accept: "application/vnd.github+json" } });
  if (!res.ok) throw new Error(String(res.status));
  return res.json();
}

async function fetchLive(): Promise<LiveRepo | null> {
  try {
    const cached = JSON.parse(sessionStorage.getItem(CACHE_KEY) ?? "null") as { at: number; data: LiveRepo } | null;
    if (cached && Date.now() - cached.at < CACHE_MS) return cached.data;
  } catch {}
  try {
    const [repo, prs, pulls] = await Promise.all([
      json(`${API}/repos/${REPO}`) as Promise<{ stargazers_count?: number; open_issues_count?: number }>,
      json(`${API}/search/issues?q=${encodeURIComponent(`repo:${REPO} is:pr is:open`)}&per_page=1`) as Promise<{ total_count?: number }>,
      json(`${API}/repos/${REPO}/pulls?state=closed&sort=updated&direction=desc&per_page=40`) as Promise<Array<{
        number: number; title: string; html_url: string; merged_at: string | null; author_association?: string;
        user: { login: string; avatar_url: string; type?: string };
      }>>,
    ]);
    const stars = repo.stargazers_count;
    const openPulls = prs.total_count;
    if (typeof stars !== "number" || stars <= 0 || typeof openPulls !== "number" || typeof repo.open_issues_count !== "number") return null;
    const merged = pulls
      .filter((pr) => pr.merged_at && pr.user.type !== "Bot" && !pr.user.login.endsWith("[bot]"))
      .sort((a, b) => +new Date(b.merged_at!) - +new Date(a.merged_at!))
      .map((pr) => ({
        number: pr.number,
        title: pr.title,
        url: pr.html_url,
        author: pr.user.login,
        avatar: pr.user.avatar_url,
        mergedAt: pr.merged_at!,
        firstTime: pr.author_association === "FIRST_TIME_CONTRIBUTOR",
      }));
    const data = { stars, openPulls, openIssues: Math.max(0, repo.open_issues_count - openPulls), merged };
    try { sessionStorage.setItem(CACHE_KEY, JSON.stringify({ at: Date.now(), data })); } catch {}
    return data;
  } catch {
    return null;
  }
}

export function useLiveRepo(enabled: boolean): LiveRepo | null {
  const [data, setData] = useState<LiveRepo | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    pending ??= fetchLive();
    pending.then((value) => { if (alive) setData(value); });
    return () => { alive = false; };
  }, [enabled]);
  return data;
}

export function LiveStarCount({ initial, numberLocale }: { initial?: number; numberLocale: string }) {
  const live = useLiveRepo(initial === undefined);
  const stars = initial ?? live?.stars;
  if (stars === undefined) return null;
  return (
    <span className="github-star-count tabular" data-live={initial === undefined ? "client" : "server"}>
      {new Intl.NumberFormat(numberLocale).format(stars)}
    </span>
  );
}

export interface PulseRow { key: "stars" | "contributors" | "pulls" | "issues"; label: string; href: string; value?: string }

export function LiveStarLink({ initial, label, href, numberLocale }: { initial?: number; label: string; href: string; numberLocale: string }) {
  const live = useLiveRepo(initial === undefined);
  const stars = initial ?? live?.stars;
  if (stars === undefined) return null;
  return (
    <a href={href} className="section-link home-proof-stars">
      <span aria-hidden="true">★</span>
      <span className="tabular">{new Intl.NumberFormat(numberLocale).format(stars)}</span>
      <span>{label}</span>
    </a>
  );
}

export function LivePulseStats({ rows, numberLocale }: { rows: PulseRow[]; numberLocale: string }) {
  const missing = rows.some((row) => row.value === undefined);
  const live = useLiveRepo(missing);
  const fmt = new Intl.NumberFormat(numberLocale);
  const fill = (row: PulseRow) => {
    if (row.value !== undefined || !live) return row.value;
    if (row.key === "stars") return fmt.format(live.stars);
    if (row.key === "pulls") return fmt.format(live.openPulls);
    if (row.key === "issues") return fmt.format(live.openIssues);
    return undefined;
  };
  return (
    <dl className="pulse-stats">
      {rows.map((row) => {
        const value = fill(row);
        if (value === undefined) return null;
        return (
          <div key={row.key}>
            <dt>{row.label}</dt>
            <dd><a href={row.href} className="tabular">{value}</a></dd>
          </div>
        );
      })}
    </dl>
  );
}

export interface MergedCopy { heading: string; mergedAge: string; firstContribution: string; dateLocale: string }

function ago(iso: string, dateLocale: string): string {
  const mins = Math.round((Date.now() - +new Date(iso)) / 60000);
  const fmt = new Intl.RelativeTimeFormat(dateLocale, { numeric: "auto" });
  if (mins < 60) return fmt.format(-Math.max(mins, 0), "minute");
  const hrs = Math.round(mins / 60);
  if (hrs < 24) return fmt.format(-hrs, "hour");
  const days = Math.round(hrs / 24);
  if (days < 30) return fmt.format(-days, "day");
  return fmt.format(-Math.round(days / 30), "month");
}

export function LiveMergedList({ limit, copy }: { limit: number; copy: MergedCopy }) {
  const live = useLiveRepo(true);
  const items = live?.merged.slice(0, limit) ?? [];
  if (items.length === 0) return null;
  return (
    <div className="pulse-main">
      <h3 className="pulse-heading"><span className="status status-live"><span className="status-dot" aria-hidden="true" />{copy.heading}</span></h3>
      <ol className="merged-list" role="list">
        {items.map((item) => (
          <li key={item.number}>
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img src={`${item.avatar}${item.avatar.includes("?") ? "&" : "?"}s=80`} alt="" width={40} height={40} loading="lazy" decoding="async" className="face" />
            <div className="merged-text">
              <a href={item.url} className="merged-title">{item.title}</a>
              <p className="merged-meta">
                <a href={`https://github.com/${item.author}`}>@{item.author}</a>
                <span>{copy.mergedAge.replace("{age}", ago(item.mergedAt, copy.dateLocale))}</span>
                {item.firstTime ? <span className="merged-first">{copy.firstContribution}</span> : null}
              </p>
            </div>
          </li>
        ))}
      </ol>
    </div>
  );
}
