"use client";

import { useMemo, useState } from "react";
import { Icon } from "./icon";
import type { ModelFact } from "@/lib/facts";
import { MODELS_COPY, formatModelTokens, formatModelDate } from "@/lib/content/models";
import { getChrome, pickText } from "@/lib/i18n/dictionaries";

type SortKey = "added" | "id" | "provider" | "context";

// Missing values sort last in BOTH directions — an undated model is never
// "newest" or "oldest", it is simply unknown.
function compare(a: ModelFact, b: ModelFact, key: SortKey, desc: boolean): number {
  const tie = a.id.localeCompare(b.id);
  switch (key) {
    case "added": {
      if (!a.addedAt || !b.addedAt) {
        if (a.addedAt === b.addedAt) return tie;
        return a.addedAt ? -1 : 1;
      }
      if (a.addedAt !== b.addedAt) {
        return desc
          ? b.addedAt.localeCompare(a.addedAt)
          : a.addedAt.localeCompare(b.addedAt);
      }
      return tie;
    }
    case "id":
      return desc ? -tie : tie;
    case "provider": {
      if (!a.provider || !b.provider) {
        if (a.provider === b.provider) return tie;
        return a.provider ? -1 : 1;
      }
      const c = a.provider.localeCompare(b.provider);
      return c !== 0 ? (desc ? -c : c) : tie;
    }
    case "context": {
      if (a.contextWindow === null || b.contextWindow === null) {
        if (a.contextWindow === b.contextWindow) return tie;
        return a.contextWindow === null ? 1 : -1;
      }
      return a.contextWindow !== b.contextWindow
        ? desc
          ? b.contextWindow - a.contextWindow
          : a.contextWindow - b.contextWindow
        : tie;
    }
  }
}

export function ModelsTable({
  models,
  locale,
}: {
  models: ModelFact[];
  locale: string;
}) {
  const [key, setKey] = useState<SortKey>("added");
  const [desc, setDesc] = useState(true);
  const t = (copy: { en: string; zh: string }) => pickText(copy, locale);
  const dateLocale = getChrome(locale).dateLocale;

  const rows = useMemo(
    () => [...models].sort((a, b) => compare(a, b, key, desc)),
    [models, key, desc],
  );

  const setSort = (next: SortKey) => {
    if (next === key) {
      setDesc(!desc);
      return;
    }
    setKey(next);
    setDesc(next === "added" || next === "context");
  };

  const headers: { key: SortKey; label: string; className?: string }[] = [
    { key: "id", label: t(MODELS_COPY.colModel) },
    { key: "provider", label: t(MODELS_COPY.colProvider) },
    { key: "context", label: t(MODELS_COPY.colContext) },
    { key: "added", label: t(MODELS_COPY.colAdded) },
  ];

  return (
    <div className="data-table-wrap">
      <table className="data-table">
        <caption className="sr-only">{t(MODELS_COPY.modelsTitle)}</caption>
        <thead>
          <tr>
            {headers.map((h) => {
              const active = key === h.key;
              const ascending = active && !desc;
              return (
                <th
                  key={h.key}
                  scope="col"
                  aria-sort={active ? (ascending ? "ascending" : "descending") : "none"}
                >
                  <button
                    type="button"
                    className="data-table-sort"
                    data-dir={active ? (ascending ? "asc" : "desc") : "none"}
                    onClick={() => setSort(h.key)}
                    title={t(ascending ? MODELS_COPY.sortDesc : MODELS_COPY.sortAsc)}
                  >
                    {h.label}
                    <Icon name="chevron-down" />
                  </button>
                </th>
              );
            })}
          </tr>
        </thead>
        <tbody>
          {rows.map((model) => (
            <tr key={model.id}>
              <th scope="row">
                <code>{model.id}</code>
                {model.reasoning ? (
                  <span className="models-reasoning">{t(MODELS_COPY.reasoning)}</span>
                ) : null}
              </th>
              <td>{model.provider ?? "—"}</td>
              <td className="tabular">{formatModelTokens(model.contextWindow)}</td>
              <td className="tabular">{formatModelDate(model.addedAt, dateLocale)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
