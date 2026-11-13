import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { resolveWhale } from "./whale-tokens";
import { siteCss } from "./site-css";

// The role layer (app/styles/tokens-roles.css): every role in every scheme,
// drawn from the codewhale-design 5.x tokens, every ink measured against
// every ground at WCAG AA.
const CSS = siteCss();
const ROLES = [
  "bg", "surface", "panel", "text", "muted", "line", "line-strong", "accent", "on-accent",
  "hover", "selected", "selection", "ring", "live", "attention", "danger",
];
const GROUNDS = ["bg", "surface", "panel", "hover", "selected"];
const TEXT_INKS = ["text", "muted", "accent", "live", "attention", "danger"];

function declarations(block: string): Record<string, string> {
  const vars: Record<string, string> = {};
  for (const match of block.matchAll(/--([\w-]+):\s*([^;]+);/g)) vars[match[1]] = match[2].trim();
  return vars;
}

/** The first block for `selector` (at any indent) that declares `--bg`. */
function roleBlock(selector: string): Record<string, string> {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  for (const match of CSS.matchAll(new RegExp(`(?:^|\\n)\\s*${escaped}\\s*\\{([^}]*)\\}`, "g"))) {
    const vars = declarations(match[1]);
    if ("bg" in vars) return vars;
  }
  throw new Error(`No role block for ${selector}`);
}

function hex(value: string): string {
  const resolved = resolveWhale(value);
  if (!/^#[0-9a-f]{6}$/i.test(resolved)) throw new Error(`not a hex color: ${value} -> ${resolved}`);
  return resolved.toLowerCase();
}

function luminance(color: string): number {
  const [r, g, b] = color
    .slice(1)
    .match(/.{2}/g)!
    .map((v) => Number.parseInt(v, 16) / 255)
    .map((v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const light = () => roleBlock(":root");
const pinnedDark = () => roleBlock(':root[data-theme="dark"]');

describe("role tokens", () => {
  it("defines every role in light and pinned-dark", () => {
    for (const [name, scheme] of [["light", light()], ["pinned", pinnedDark()]] as const) {
      for (const role of ROLES) expect(scheme, `${name}.${role}`).toHaveProperty(role);
    }
    // Light is the site's appearance: dark is a reader's pin, never the OS.
    expect(CSS).not.toMatch(/@media \(prefers-color-scheme: dark\)/);
  });

  it("takes every role from the codewhale-design 5.x light and dark tokens", () => {
    const gpui = (mode: string, name: string) => hex(`var(--gpui-${mode}-${name})`);
    const inks: Record<string, Record<string, string>> = {
      light: { bg: "bg", panel: "panel", surface: "sidebar", text: "text", muted: "text-muted", line: "border", "line-strong": "border-strong", accent: "primary", "on-accent": "on-primary", ring: "accent", live: "live", attention: "attention", danger: "danger", hover: "hover", selected: "list-active" },
      dark: { bg: "bg", panel: "sidebar", surface: "panel", text: "text", muted: "text-muted", line: "border", "line-strong": "border-strong", accent: "accent", ring: "accent", live: "live", attention: "attention", danger: "danger", hover: "hover", selected: "list-active" },
    };
    for (const [mode, scheme] of [["light", light()], ["dark", pinnedDark()]] as const) {
      for (const [role, name] of Object.entries(inks[mode])) {
        expect(scheme[role], `${mode}.${role}`).toBe(`var(--gpui-${mode}-${name})`);
        expect(hex(scheme[role]), `${mode}.${role}`).toBe(gpui(mode, name));
      }
    }
    expect(hex(light().bg)).toBe("#f1f5f9");
    expect(hex(pinnedDark().bg)).toBe("#030508");
    expect(CSS).not.toMatch(/--kit-(warm-paper|foam|mist|bioluminescence|ink|slate)/);
  });

  it("re-declares no role for the footer: it sits on the page's own ground", () => {
    expect(CSS).not.toMatch(/\.stage\b/);
    expect(CSS).not.toMatch(/\.site-footer\s*\{[^}]*--(bg|text|muted):/);
    expect(CSS).not.toMatch(/--sea(-[\w-]+)?:/);
  });

  it("keeps every text ink at WCAG AA on every ground, in every scheme", () => {
    for (const [name, scheme] of [["light", light()], ["dark", pinnedDark()]] as const) {
      for (const ground of GROUNDS) {
        for (const ink of TEXT_INKS) {
          expect(contrast(hex(scheme[ink]), hex(scheme[ground])), `${name}: ${ink} on ${ground}`).toBeGreaterThanOrEqual(4.5);
        }
      }
      // Control edges and state marks clear the 3:1 non-text floor.
      for (const ground of ["bg", "panel"]) {
        expect(contrast(hex(scheme["line-strong"]), hex(scheme[ground])), `${name}: line-strong on ${ground}`).toBeGreaterThanOrEqual(3);
      }
      expect(contrast(hex(scheme["on-accent"]), hex(scheme.accent)), `${name}: on-accent`).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("puts white text on the logo gradient only where it clears 4.5:1", () => {
    const fill = CSS.match(/--brand-fill:\s*linear-gradient\(([^;]+)\);/)?.[1] ?? "";
    const stops = [...fill.matchAll(/#[0-9a-f]{6}|var\(--brand-[\w-]+\)/gi)].map((m) => hex(m[0]));
    expect(stops.length).toBeGreaterThanOrEqual(2);
    for (const stop of stops) expect(contrast("#ffffff", stop), stop).toBeGreaterThanOrEqual(4.5);
    expect(hex("var(--brand-deep)")).toBe("#0b48bb");
    expect(hex("var(--brand-light)")).toBe("#1e8fd8");
  });

  it("keeps the home's text readable across the water, in both schemes", () => {
    const tokens = readFileSync(new URL("../app/tokens.css", import.meta.url), "utf8");
    for (const [mode, scheme] of [["light", light()], ["dark", pinnedDark()]] as const) {
      expect(scheme["home-sky"], mode).toContain(`var(--gpui-surface-light-${mode})`);
      const ramp = tokens.match(new RegExp(`--gpui-water-${mode}:\\s*linear-gradient\\(([^;]+)\\);`))?.[1] ?? "";
      const stops = [...ramp.matchAll(/#[0-9a-f]{6}/gi)].map((m) => m[0].toLowerCase());
      expect(stops.length, mode).toBeGreaterThanOrEqual(3);
      for (const stop of stops) {
        for (const ink of ["text", "muted", "live"]) {
          expect(contrast(hex(scheme[ink]), stop), `${mode}: ${ink} on ${stop}`).toBeGreaterThanOrEqual(4.5);
        }
      }
      const band = [...CSS.matchAll(/--home-band: (#[0-9a-f]{6});/g)].map((m) => m[1])[mode === "light" ? 0 : 1];
      expect(stops, `${mode} band`).toContain(band);
    }
  });
});
