import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { siteCss } from "./site-css";

const CSS = siteCss();

function selectorBlock(selector: string): string {
  const match = CSS.match(new RegExp(`(?:^|\n)${selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\{([^}]*)\\}`, "s"));
  if (!match) throw new Error(`Missing CSS selector: ${selector}`);
  return match[1];
}




const PINNED_DARK = ':root[data-theme="dark"]';

function allVars(selector: string): Record<string, string> {
  return Object.fromEntries(
    [...selectorBlock(selector).matchAll(/--([\w-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]),
  );
}

describe("site-wide theme contract", () => {
  it("is paper by default; dark is a reader's pin, never the OS", () => {
    // No region-scoped dark sheet: the docs portal follows the root theme.
    expect(CSS).not.toMatch(/html\[data-theme="dark"\] \.docs-(portal|theme)/);
    expect(CSS).toMatch(/(?:^|\n):root \{[^}]*color-scheme:\s*light;/);
    expect(CSS).not.toMatch(/@media \(prefers-color-scheme: dark\)/);
    expect(Object.keys(allVars(PINNED_DARK))).toContain("bg");
    expect(selectorBlock(PINNED_DARK)).toMatch(/color-scheme:\s*dark/);
  });

  it("re-inks the nav wordmark from the mark ink in every appearance", () => {
    // wordmark.svg is fixed #142352 ink; the header draws it as a mask so
    // --mark-ink (the page's text colour) paints it on paper and on the ocean.
    expect(CSS).toMatch(/\.paper-wordmark-logo,\s*\.wordmark\s*\{[^}]*background: var\(--mark-ink\);/);
  });

  it("shows the toggle on every page with one light|dark storage contract", () => {
    const toggle = readFileSync(new URL("../components/theme-toggle.tsx", import.meta.url), "utf8");
    expect(toggle).not.toMatch(/isDocsPath/);
    expect(toggle).toMatch(/type Mode = "light" \| "dark"/);
    expect(toggle).toContain('const KEY = "cw-theme"');
    // The boot script pins only an explicit light/dark; anything else (a
    // legacy "system" or "auto", nothing) stays on paper.
    expect(toggle).toContain("localStorage.getItem('cw-theme');if(t==='light'||t==='dark')");
  });
});

describe("docs theme contrast contract", () => {
  // The docs read the legacy names; they must resolve to the roles, whose
  // contrast gpui-role-tokens.test.ts checks in every scheme.
  it("points the docs accent and buttons at the role tokens", () => {
    const root = Object.fromEntries(
      [...CSS.matchAll(/(?:^|\n):root \{([^}]*)\}/g)].flatMap((m) =>
        [...m[1].matchAll(/--([\w-]+):\s*([^;]+);/g)].map((v) => [v[1], v[2].trim()]),
      ),
    );
    expect(root["docs-accent"]).toBe("var(--accent)");
    expect(root["docs-button-text"]).toBe("var(--text)");
    expect(root["docs-button-bg"]).toBe("var(--panel)");
    expect(CSS).toMatch(/\.docs-sidebar-link:hover,\s*\.docs-sidebar-link-current\s*{[^}]*color:\s*var\(--docs-accent\)/s);
    expect(selectorBlock(".docs-theme .portal-button-secondary")).toContain("color: var(--docs-button-text)");
  });
});
