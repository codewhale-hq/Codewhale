"use client";

/**
 * <ThemeToggle> — an icon-only Light / Dark control in the site nav, shown
 * on every page. The current mode is in its accessible name.
 *
 * Paper is the site's appearance (paper above, sea below the horizon), so
 * the site no longer follows the OS. Dark pins the navy ground through
 * `data-theme="dark"`; light removes the pin.
 *
 * One storage contract, shared with the web app: the `cw-theme` key holds
 * `light | dark`. A stored `system` or `auto` (earlier modes) reads as
 * light. The server-inserted boot script applies a stored pin before paint;
 * locale navigation reapplies that choice from the same storage contract.
 */

import { useLayoutEffect, useRef, useState } from "react";
import { useServerInsertedHTML } from "next/navigation";
import { fill } from "@/lib/i18n/dictionaries";
import { Icon, type IconName } from "./icon";

type Mode = "light" | "dark";
const ORDER: Mode[] = ["light", "dark"];
const KEY = "cw-theme";

function load(): Mode {
  try {
    const stored = localStorage.getItem(KEY);
    return stored === "dark" ? "dark" : "light";
  } catch {
    return "light";
  }
}

function apply(mode: Mode) {
  const el = document.documentElement;
  if (mode === "light") el.removeAttribute("data-theme");
  else el.setAttribute("data-theme", mode);
  try {
    localStorage.setItem(KEY, mode);
  } catch {
    /* private mode / storage disabled — the choice applies until reload */
  }
}

export function ThemeToggle({
  lightLabel,
  darkLabel,
  ariaTemplate,
}: {
  lightLabel: string;
  darkLabel: string;
  /** "Theme: {mode} (click to cycle)" — interpolated with fill(). */
  ariaTemplate: string;
}) {
  const [mode, setMode] = useState<Mode>("light");
  const [mounted, setMounted] = useState(false);

  const inserted = useRef(false);
  useServerInsertedHTML(() => {
    if (inserted.current) return null;
    inserted.current = true;
    return (
      <script
        dangerouslySetInnerHTML={{
          __html:
            "(function(){try{var t=localStorage.getItem('cw-theme');if(t==='light'||t==='dark'){document.documentElement.setAttribute('data-theme',t);}}catch(e){}})();",
        }}
      />
    );
  });

  useLayoutEffect(() => {
    const selected = load();
    apply(selected);
    setMode(selected);
    setMounted(true);
  }, []);

  const cycle = () => {
    const next = ORDER[(ORDER.indexOf(mode) + 1) % ORDER.length];
    setMode(next);
    apply(next);
  };

  const labels: Record<Mode, string> = {
    light: lightLabel,
    dark: darkLabel,
  };
  const glyph: Record<Mode, IconName> = { light: "sun", dark: "moon" };
  const shown = mounted ? mode : "light";

  return (
    <button
      type="button"
      onClick={cycle}
      className="nav-icon-button"
      aria-label={fill(ariaTemplate, { mode: labels[shown] })}
      suppressHydrationWarning
    >
      <Icon name={glyph[shown]} className="nav-icon" />
    </button>
  );
}
