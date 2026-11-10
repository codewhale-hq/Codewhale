"use client";

import { useState } from "react";
import { Icon } from "./icon";
import { recordUsage } from "./usage-counting";

interface Props {
  cmd: string;
  copyLabel?: string;
  copiedLabel?: string;
  copyLocale?: string;
  installIntent?: boolean;
}

/**
 * A command in a code block with one button that copies it: the copy icon
 * and the word, so the action reads without guessing at the glyph. The check
 * mark, "Copied" and its announcement appear only after the clipboard write
 * succeeds; a failed write never shows a false confirmation.
 */
export function InstallCodeBlock({ cmd, copyLabel = "Copy", copiedLabel = "Copied", copyLocale, installIntent = false }: Props) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    if (typeof navigator === "undefined" || !navigator.clipboard) return;
    try {
      await navigator.clipboard.writeText(cmd);
      setCopied(true);
      if (installIntent) recordUsage("install_copy");
      setTimeout(() => setCopied(false), 1400);
    } catch {
      // Clipboard write failed (permissions, non-secure context, or a focused
      // write race) — never show a false "Copied" confirmation.
    }
  };

  const copiedWord = copiedLabel.replace(/\s*✓\s*$/u, "");

  return (
    <div className="code">
      <pre tabIndex={0} className="code-block">{cmd}</pre>
      <button
        type="button"
        lang={copyLocale}
        onClick={copy}
        data-copied={copied}
        className="btn btn-sm code-copy"
      >
        <Icon name={copied ? "check" : "copy"} className="icon" />
        <span className="code-copy-label">{copied ? copiedWord : copyLabel}</span>
      </button>
      <span className="sr-only" aria-live="polite" lang={copyLocale}>
        {/* The check mark is drawn by the icon; announce only the word. */}
        {copied ? copiedWord : ""}
      </span>
    </div>
  );
}
