"use client";

import Link from "next/link";
import { useEffect, useMemo, useRef, useState } from "react";
import { Icon } from "@/components/icon";
import { rankDocSections, type DocSearchSection } from "@/lib/doc-search-match";
import { highlightSpan } from "@/lib/search-utils";

interface Labels { search: string; placeholder: string; empty: string; none: string; failed: string; close: string }

function Marked({ text, query }: { text: string; query: string }) {
  const span = highlightSpan(text, query.trim().split(/\s+/)[0] ?? "");
  return span ? <>{span.before}<mark>{span.match}</mark>{span.after}</> : <>{text}</>;
}

export function DocsPreviewSearch({ locale, indexHref, labels }: { locale: string; indexHref: string; labels: Labels }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [sections, setSections] = useState<DocSearchSection[] | null>(null);
  const [failed, setFailed] = useState(false);
  const results = useMemo(() => (sections ? rankDocSections(sections, query) : []), [sections, query]);

  const open = () => {
    dialog.current?.showModal();
    if (sections || failed) return;
    fetch(indexHref)
      .then((response) => (response.ok ? response.json() : Promise.reject(new Error(String(response.status)))))
      .then((body: { sections: DocSearchSection[] }) => setSections(body.sections))
      .catch(() => setFailed(true));
  };

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        if (dialog.current?.open) dialog.current.close();
        else open();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const onInputKey = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setActive((current) => (results.length ? (current + (event.key === "ArrowDown" ? 1 : results.length - 1)) % results.length : 0));
    } else if (event.key === "Enter" && results[active]) {
      event.preventDefault();
      dialog.current?.querySelector<HTMLAnchorElement>(`[data-result="${active}"]`)?.click();
    }
  };

  const term = query.trim();

  return (
    <>
      <button type="button" className="dp-search-trigger" onClick={open} aria-haspopup="dialog">
        <Icon name="search" className="icon" />
        <span>{labels.placeholder}</span>
        <kbd aria-hidden="true">⌘K</kbd>
      </button>
      <dialog ref={dialog} className="dp-search" aria-label={labels.search} onClick={(event) => { if (event.target === dialog.current) dialog.current?.close(); }} onClose={() => { setQuery(""); setActive(0); }}>
        <div className="dp-search-panel">
          <div className="dp-search-field">
            <Icon name="search" className="icon" />
            <input
              type="search"
              autoFocus
              value={query}
              onChange={(event) => { setQuery(event.target.value); setActive(0); }}
              onKeyDown={onInputKey}
              placeholder={labels.placeholder}
              aria-label={labels.search}
              role="combobox"
              aria-expanded={results.length > 0}
              aria-controls="dp-search-results"
              aria-activedescendant={results[active] ? `dp-search-result-${active}` : undefined}
            />
            <button type="button" className="btn btn-sm btn-ghost" onClick={() => dialog.current?.close()}>{labels.close}</button>
          </div>
          <ul id="dp-search-results" role="listbox" aria-label={labels.search} className="dp-search-results">
            {results.map((result, index) => (
              <li key={`${result.href}-${index}`} id={`dp-search-result-${index}`} role="option" aria-selected={index === active} lang={result.lang}>
                <Link href={`/${locale}${result.href}`} data-result={index} onClick={() => dialog.current?.close()} onMouseMove={() => setActive(index)}>
                  <span className="dp-search-crumb">{result.pageTitle}</span>
                  <span className="dp-search-title"><Marked text={result.title} query={term} /></span>
                  <span className="dp-search-excerpt"><Marked text={result.excerpt} query={term} /></span>
                </Link>
              </li>
            ))}
          </ul>
          <p className="dp-search-status" role="status">
            {failed ? labels.failed : !term ? labels.empty : sections && !results.length ? labels.none.replace("{query}", term) : ""}
          </p>
        </div>
      </dialog>
    </>
  );
}
