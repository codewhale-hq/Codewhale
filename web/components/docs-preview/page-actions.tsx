"use client";

import { useState } from "react";
import { Icon } from "@/components/icon";

export function DocsPreviewPageActions({ markdownHref, sourceHref, labels }: { markdownHref: string; sourceHref: string; labels: { copyPage: string; copied: string; viewMarkdown: string; source: string } }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    if (typeof navigator === "undefined" || !navigator.clipboard) return;
    try {
      const response = await fetch(markdownHref);
      if (!response.ok) return;
      await navigator.clipboard.writeText(await response.text());
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div className="dp-actions">
      <button type="button" className="btn btn-sm btn-secondary" onClick={copy} data-copied={copied}>
        <Icon name={copied ? "check" : "copy"} className="icon" />
        <span>{copied ? labels.copied : labels.copyPage}</span>
      </button>
      <a className="btn btn-sm btn-ghost" href={markdownHref}>
        <Icon name="terminal" className="icon" />
        <span>{labels.viewMarkdown}</span>
      </a>
      <a className="btn btn-sm btn-ghost" href={sourceHref} target="_blank" rel="noreferrer">
        <Icon name="github" className="icon" />
        <span>{labels.source}</span>
      </a>
      <span className="sr-only" aria-live="polite">{copied ? labels.copied : ""}</span>
    </div>
  );
}
