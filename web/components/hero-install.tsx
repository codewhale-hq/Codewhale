"use client";

import { useState } from "react";
import { INSTALL_COMMANDS, INSTALL_COPY } from "@/lib/content/install";
import { pickText, pickTextLocale } from "@/lib/i18n/dictionaries";
import { InstallCodeBlock } from "./install-code-block";

/**
 * The hero's copyable install plate: one segmented choice between the
 * checked shell installer (macOS and Linux), winget on Windows, npm, and Cargo,
 * above the same copy block the install page uses. The option
 * labels are code-owned proper nouns; only the group's accessible name is
 * translated.
 */
const OPTIONS = [
  { id: "shell", label: "macOS · Linux", cmd: INSTALL_COMMANDS.shell },
  { id: "windows", label: "Windows", cmd: INSTALL_COMMANDS.windows },
  { id: "npm", label: "npm", cmd: INSTALL_COMMANDS.npm },
  { id: "cargo", label: "Cargo", cmd: INSTALL_COMMANDS.cargo },
] as const;

export function HeroInstall({
  locale,
  ariaLabel,
  copyLabel,
  copiedLabel,
}: {
  locale: string;
  ariaLabel: string;
  copyLabel: string;
  copiedLabel: string;
}) {
  const [selected, setSelected] = useState<(typeof OPTIONS)[number]["id"]>("shell");
  const option = OPTIONS.find((o) => o.id === selected) ?? OPTIONS[0];
  const note = selected === "windows" ? INSTALL_COPY.windowsNote : selected === "cargo" ? INSTALL_COPY.cargoNote : null;

  return (
    <div className="hero-install plate">
      <div className="segmented" role="group" aria-label={ariaLabel}>
        {OPTIONS.map((o) => (
          <button
            key={o.id}
            type="button"
            onClick={() => setSelected(o.id)}
            aria-pressed={o.id === selected}
          >
            {o.label}
          </button>
        ))}
      </div>
      <InstallCodeBlock installIntent cmd={option.cmd} copyLabel={copyLabel} copiedLabel={copiedLabel} />
      {note ? <p className="install-head-note" lang={pickTextLocale(locale)}>{pickText(note, locale)}</p> : null}
    </div>
  );
}
