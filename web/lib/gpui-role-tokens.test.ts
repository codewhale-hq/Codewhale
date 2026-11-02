import { describe, expect, it } from "vitest";
import { resolveWhale } from "./whale-tokens";
import { siteCss } from "./site-css";

// The role layer (app/styles/tokens-roles.css): every role in every scheme,
// every value a tokens.json role, every ink measured against every
// ground at WCAG AA.
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
const stage = () => roleBlock(".stage,\n.site-footer");

describe("role tokens", () => {
  it("defines every role in light, pinned-dark and the stage", () => {
    for (const [name, scheme] of [["light", light()], ["pinned", pinnedDark()], ["stage", stage()]] as const) {
      for (const role of ROLES) expect(scheme, `${name}.${role}`).toHaveProperty(role);
    }
    // Paper is the site's appearance: dark is a reader's pin, never the OS.
    expect(CSS).not.toMatch(/@media \(prefers-color-scheme: dark\)/);
  });

  it("inks every role from the tokens.json set in both appearances", () => {
    const inks: Record<string, string> = {
      text: "text", muted: "text-muted", accent: "accent", ring: "accent", live: "live",
      attention: "attention", danger: "danger", "on-accent": "on-primary",
      bg: "bg", panel: "panel", surface: "sidebar", hover: "hover", selected: "list-active",
      "brand-fill": "primary", "on-brand": "on-primary",
    };
    for (const [mode, scheme, isStage] of [["light", light(), false], ["dark", pinnedDark(), false], ["dark", stage(), true]] as const) {
      for (const [role, token] of Object.entries(inks)) {
        if (isStage && role === "bg") continue;
        expect(scheme[role], `${mode}.${role}`).toBe(`var(--gpui-${mode}-${token})`);
      }
    }
    expect(stage().bg).toBe("var(--gpui-dark-ocean-depth)");
  });

  it("keeps every text ink at WCAG AA on every ground, in every scheme", () => {
    for (const [name, scheme] of [["light", light()], ["dark", pinnedDark()], ["stage", stage()]] as const) {
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

  it("puts primary-foreground ink on every filled action at AA", () => {
    for (const [name, scheme] of [["light", light()], ["dark", pinnedDark()], ["stage", stage()]] as const) {
      expect(contrast(hex(scheme["on-brand"]), hex(scheme["brand-fill"])), `${name}: on-brand`).toBeGreaterThanOrEqual(4.5);
    }
    expect(hex("var(--brand-deep)")).toBe("#0b48bb");
    expect(hex("var(--brand-light)")).toBe("#1e8fd8");
  });

  it("keeps stage text readable on the floor below the horizon", () => {
    const floor = hex("var(--ocean-floor)");
    expect(contrast(hex(stage().text), floor)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(hex(stage().muted), floor)).toBeGreaterThanOrEqual(4.5);
  });
});
