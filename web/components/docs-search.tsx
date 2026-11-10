"use client";

import { useState, useMemo, useRef, useCallback, useEffect } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { searchDocSections, type DocSearchSection } from "@/lib/doc-search-match";
import {
  DOC_CATEGORY_LABELS,
  DOC_TOPICS,
  docTopicHref,
  docTopicIsExternal,
  type DocTopic,
} from "@/lib/docs-map";
import { DOC_TASKS, docTaskHaystack, type DocTask } from "@/lib/docs-tasks";
import { fill, getDocsShell, getFeatures, pickText } from "@/lib/i18n/dictionaries";
import { FEATURE_CATALOGUE } from "@/lib/features";
import { Status } from "./status-badge";
import { readBoundedBody } from "@/lib/bounded-body";
import { recordFunnel } from "./usage-counting";
import { docTopicHaystack, highlightSpan } from "@/lib/search-utils";
import { isSlashShortcut } from "@/lib/slash-shortcut";
import { EmptyState } from "./surface-state";

/* ------------------------------------------------------------------ */
/*  Helpers                                                            */
/* ------------------------------------------------------------------ */

function topicSources(topic: DocTopic): string[] {
  return Array.isArray(topic.repoSource) ? topic.repoSource : [topic.repoSource];
}

function highlight(text: string, query: string): React.ReactNode {
  // Index arithmetic lives in search-utils: lowercasing can change a
  // string's length, so `text` cannot be sliced with indices taken from
  // its lowercased copy.
  const span = highlightSpan(text, query);
  if (!span) return text;
  return (
    <>
      {span.before}
      <mark className="search-highlight">{span.match}</mark>
      {span.after}
    </>
  );
}

/* ------------------------------------------------------------------ */
/*  Rows                                                               */
/* ------------------------------------------------------------------ */

function TaskRow({ task, locale, query, onSelect }: { task: DocTask; locale: string; query: string; onSelect?: () => void }) {
  return (
    <Link href={`/${locale}${task.href}`} className="docs-topic-row docs-task-row" onClick={onSelect}>
      <div className="docs-topic-main">
        <div className="docs-topic-title">{highlight(pickText(task.label, locale), query)}</div>
        <p>{highlight(pickText(task.description, locale), query)}</p>
      </div>
      <span className="docs-topic-arrow" aria-hidden="true">→</span>
    </Link>
  );
}

function TopicRow({
  topic,
  locale,
  query,
  webGuideTag,
  sourceDocTag,
  sourceDetails,
  onSelect,
}: {
  topic: DocTopic;
  locale: string;
  query: string;
  webGuideTag: string;
  sourceDocTag: string;
  sourceDetails: string;
  onSelect?: () => void;
}) {
  const href = docTopicHref(topic, locale);
  const isExternal = docTopicIsExternal(topic);
  const sources = topicSources(topic);

  // The row is a link; its provenance (the repository files the entry is
  // written from) sits beside it behind a disclosure, so it is there for
  // maintainers without crowding the index.
  return (
    <div className="docs-topic-item">
      <Link
        href={href}
        target={isExternal ? "_blank" : undefined}
        rel={isExternal ? "noreferrer" : undefined}
        className="docs-topic-row"
        onClick={onSelect}
      >
        <div className="docs-topic-main">
          <div className="docs-topic-title">
            {highlight(pickText(topic.label, locale), query)}
            <span>{isExternal ? sourceDocTag : webGuideTag}</span>
          </div>
          <p>{highlight(pickText(topic.description, locale), query)}</p>
        </div>
        <span className="docs-topic-arrow" aria-hidden="true">{isExternal ? "↗" : "→"}</span>
      </Link>
      <details className="docs-topic-details">
        <summary>{sourceDetails}</summary>
        <p className="docs-topic-source">
          {sources.map((s, i) => (
            <span key={s}>
              {i > 0 && ", "}
              <code>{highlight(s, query)}</code>
            </span>
          ))}
        </p>
      </details>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  Main component                                                     */
/* ------------------------------------------------------------------ */

/**
 * The docs search combines rendered article sections, tasks and topics.
 * Topic and task metadata match both English and Chinese; article sections
 * follow the active dictionary. Every string is dictionary-driven.
 */
function isSearchSection(value: unknown): value is DocSearchSection {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const row = value as Record<string, unknown>;
  if (Object.keys(row).some((key) => !["href", "pageTitle", "title", "text", "lang"].includes(key))) return false;
  return typeof row.href === "string" && /^\/(?:docs|install|models|plugins|constitution)(?:[\/#]|$)/.test(row.href)
    && typeof row.pageTitle === "string" && typeof row.title === "string" && typeof row.text === "string"
    && (row.lang === undefined || (typeof row.lang === "string" && row.lang.length <= 35));
}

export function DocsSearch({ locale, compact = false }: { locale: string; compact?: boolean }) {
  const t = getDocsShell(locale);
  const [query, setQuery] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  const topicHaystacks = useMemo(() => DOC_TOPICS.map(docTopicHaystack), []);
  const taskHaystacks = useMemo(() => DOC_TASKS.map(docTaskHaystack), []);

  const [loaded, setLoaded] = useState<{ locale: string; sections: DocSearchSection[] } | null>(null);
  const [loadingLocale, setLoadingLocale] = useState<string | null>(null);
  const [errorLocale, setErrorLocale] = useState<string | null>(null);
  const request = useRef<{ locale: string; controller: AbortController } | null>(null);
  const ready = loaded?.locale === locale;
  const sections = useMemo(() => loaded?.locale === locale ? loaded.sections : [], [loaded, locale]);
  const loadSections = useCallback(async () => {
    if (loaded?.locale === locale || request.current?.locale === locale) return;
    request.current?.controller.abort();
    const controller = new AbortController();
    request.current = { locale, controller };
    setLoadingLocale(locale);
    setErrorLocale(null);
    try {
      const response = await fetch(`/api/docs/search/${encodeURIComponent(locale)}`, {
        credentials: "omit", referrerPolicy: "no-referrer",
        signal: AbortSignal.any([controller.signal, AbortSignal.timeout(8000)]),
      });
      if (!response.ok) throw new Error("unavailable");
      const bytes = await readBoundedBody(response, 2 * 1024 * 1024);
      const payload: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
      const entries = payload && typeof payload === "object" ? (payload as { sections?: unknown }).sections : null;
      if (!Array.isArray(entries) || entries.length === 0 || entries.length > 10000 || !entries.every(isSearchSection)) throw new Error("invalid index");
      if (!controller.signal.aborted) setLoaded({ locale, sections: entries });
    } catch {
      if (!controller.signal.aborted) setErrorLocale(locale);
    } finally {
      if (request.current?.controller === controller) { request.current = null; setLoadingLocale(null); }
    }
  }, [locale, loaded]);
  useEffect(() => () => request.current?.controller.abort(), []);
  const sectionMatches = useMemo(() => searchDocSections(sections, query), [sections, query]);
  const q = query.trim().toLowerCase();
  const filteredTasks = useMemo(
    () => (q ? DOC_TASKS.filter((_, i) => taskHaystacks[i].includes(q)) : DOC_TASKS),
    [q, taskHaystacks],
  );
  const filteredTopics = useMemo(
    () => (q ? DOC_TOPICS.filter((_, i) => topicHaystacks[i].includes(q)) : DOC_TOPICS),
    [q, topicHaystacks],
  );
  const filteredFeatures = useMemo(() => q ? FEATURE_CATALOGUE.features.filter((feature) =>
    [feature.id, feature.name, feature.summary, feature.flag, feature.docs, ...Object.entries(feature.surfaces).flat()].join(" ").toLowerCase().includes(q)
  ) : [], [q]);

  // Group filtered topics by category (preserve DOC_TOPICS order).
  const grouped = useMemo(() => {
    const map = new Map<DocTopic["category"], DocTopic[]>();
    for (const topic of filteredTopics) {
      const group = map.get(topic.category) ?? [];
      group.push(topic);
      map.set(topic.category, group);
    }
    return map;
  }, [filteredTopics]);

  // Keyboard shortcut: focus search on an unmodified "/" typed outside a field.
  const handleKeyDown = useCallback((e: KeyboardEvent) => {
    if (isSlashShortcut(e)) {
      e.preventDefault();
      inputRef.current?.focus();
    }
  }, []);

  useEffect(() => {
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [handleKeyDown]);

  const total = DOC_TOPICS.length + DOC_TASKS.length + FEATURE_CATALOGUE.features.length + sections.length;
  const matched = filteredTopics.length + filteredTasks.length + filteredFeatures.length + sectionMatches.length;
  const hasQuery = q.length > 0;
  const measuredQuery = useRef<string | null>(null);
  const measureSearch = useCallback(() => {
    if (!q || !ready) return;
    const key = `${locale}:${q}`;
    if (measuredQuery.current === key) return;
    measuredQuery.current = key;
    recordFunnel("docs_search");
    if (matched === 0) recordFunnel("docs_search_empty");
  }, [q, matched, ready, locale]);
  useEffect(() => {
    if (!q) { measuredQuery.current = null; return; }
    if (!ready) return;
    const timer = setTimeout(measureSearch, 400);
    return () => clearTimeout(timer);
  }, [q, ready, measureSearch]);
  const selectSearchResult = () => {
    if (!q || !ready) return;
    measureSearch();
    recordFunnel("docs_search_result");
    if (compact) setQuery("");
  };
  useEffect(() => { if (q) void loadSections(); }, [q, loadSections]);

  return (
    <div className="docs-index">
      {/* Search bar */}
      <div className="docs-search-block">
        <label htmlFor="docs-search" className="docs-search-label">
          {t.searchLabel}
        </label>
        <div className="relative">
          <input
            id="docs-search"
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onFocus={() => void loadSections()}
            aria-keyshortcuts="/"
            placeholder={t.searchPlaceholder}
            className="search-input docs-search-input w-full"
            aria-label={t.searchLabel}
            autoComplete="off"
          />
          {hasQuery && (
            <button
              type="button"
              onClick={() => setQuery("")}
              className="docs-search-clear"
              aria-label={t.searchClear}
            >
              ✕
            </button>
          )}
        </div>
        {hasQuery && ready && (
          <div className="docs-search-count" aria-live="polite">
            {matched > 0
              ? fill(t.searchMatches, { matched, total, query: query.trim() })
              : fill(t.searchNoMatches, { query: query.trim() })}
          </div>
        )}
      </div>

      {loadingLocale === locale && <p role="status">{t.searchLoading}</p>}
      {errorLocale === locale && <div role="status"><p>{t.searchUnavailable}</p><button type="button" className="btn btn-secondary" onClick={() => void loadSections()}>{t.searchRetry}</button></div>}
      {(!compact || hasQuery) && (!hasQuery || ready) && (matched > 0 ? (
        <div className="docs-result-groups">
          {sectionMatches.length > 0 && <section className="docs-result-group">
            <div className="docs-result-heading"><h2>{t.searchLabel}</h2><span>{sectionMatches.length}</span></div>
            <div className="docs-topic-list">{sectionMatches.map((section) => (
              <Link key={section.href} href={`/${locale}${section.href}`} className="docs-topic-row" onClick={selectSearchResult}>
                <div className="docs-topic-main">
                  <div className="docs-topic-title">{highlight(`${section.pageTitle} · ${section.title}`, query)}</div>
                  <p lang={section.lang}>{highlight(section.excerpt, query)}</p>
                </div>
                <span className="docs-topic-arrow" aria-hidden="true">→</span>
              </Link>
            ))}</div>
          </section>}
          {filteredFeatures.length > 0 && <section className="docs-result-group">
            <div className="docs-result-heading"><h2>{getFeatures(locale).title}</h2><span>{filteredFeatures.length}</span></div>
            <div className="docs-topic-list">{filteredFeatures.map((feature) => <Link key={feature.id} href={`/${locale}/features#${feature.id}`} className="docs-topic-row" onClick={selectSearchResult}>
              <div className="docs-topic-main" lang="en" dir="ltr"><div className="docs-topic-title">{highlight(feature.name, query)}</div><p>{highlight(feature.summary, query)}</p></div>
              <Status tone={feature.status === "stable" ? "ready" : "attention"}>{getFeatures(locale)[feature.status]}</Status>
            </Link>)}</div>
          </section>}
          {/* Tasks — "I am trying to…" */}
          {filteredTasks.length > 0 && (
            <section id="tasks" className="docs-result-group docs-task-group">
              <div className="docs-result-heading">
                <h2>{t.tasksHeading}</h2>
                <span>{filteredTasks.length}</span>
              </div>
              {!hasQuery && <p className="docs-result-lead">{t.tasksLead}</p>}
              <div className="docs-topic-list">
                {filteredTasks.map((task) => (
                  <TaskRow key={task.id} task={task} locale={locale} query={query} onSelect={hasQuery ? selectSearchResult : undefined} />
                ))}
              </div>
            </section>
          )}

          {/* Topics by category */}
          {grouped.size > 0 && (
            <div className="docs-result-topics">
              {!hasQuery && (
                <div className="docs-result-heading docs-result-heading-topics">
                  <h2>{t.topicsHeading}</h2>
                  <span>{filteredTopics.length}</span>
                </div>
              )}
              {[...grouped.entries()].map(([category, topics]) => (
                <section key={category} id={category} className="docs-result-group">
                  <div className="docs-result-heading">
                    <h3>{pickText(DOC_CATEGORY_LABELS[category], locale)}</h3>
                    <span>{topics.length}</span>
                  </div>
                  <div className="docs-topic-list">
                    {topics.map((topic) => (
                      <TopicRow
                        key={topic.id}
                        topic={topic}
                        locale={locale}
                        query={query}
                        webGuideTag={t.webGuideTag}
                        sourceDocTag={t.sourceDocTag}
                        sourceDetails={t.sourceDetails}
                      onSelect={hasQuery ? selectSearchResult : undefined}
                      />
                    ))}
                  </div>
                </section>
              ))}
            </div>
          )}
        </div>
      ) : (
        <EmptyState
          locale={locale}
          title={t.emptyTitle}
          body={t.emptyBody}
          action={
            <a
              href="https://github.com/codewhale-hq/CodeWhale/tree/main/docs"
              target="_blank"
              rel="noreferrer"
              className="portal-button portal-button-secondary"
            >
              {t.emptyCta}
            </a>
          }
        />
      ))}

      {/* Registry note (only when not searching) */}
      {!hasQuery && !compact && (
        <section className="docs-source-note">
          <p>{t.indexNote}</p>
        </section>
      )}
    </div>
  );
}

export function DocsArticleSearch({ locale }: { locale: string }) {
  const pathname = usePathname();
  if (pathname.replace(/\/+$/, "") === `/${locale}/docs`) return null;
  return <div className="mb-8"><DocsSearch locale={locale} compact /></div>;
}
