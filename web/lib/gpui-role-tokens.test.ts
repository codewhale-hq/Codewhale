import { describe, expect, it } from "vitest";
import { resolveWhale } from "./whale-tokens";
import { siteCss } from "./site-css";

// The role layer (app/styles/tokens-roles.css): every role in every scheme,
// inks from the Moda brand kit, the navy ocean grounds measured
// against every ink at WCAG AA.
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

  it("inks with the Moda brand kit in both appearances", () => {
    const kit = (name: string) => hex(`var(--kit-${name})`);
    const inks: Record<string, Record<string, string>> = {
      light: { text: "ink", muted: "slate", accent: "signal-blue", ring: "signal-blue", live: "kelp-deep", attention: "amber", danger: "rust", "on-accent": "white" },
      dark: { text: "foam", muted: "mist", accent: "bioluminescence", ring: "bioluminescence", live: "kelp", attention: "lantern", danger: "coral", "on-accent": "abyss" },
    };
    for (const [mode, scheme] of [["light", light()], ["dark", pinnedDark()], ["dark", stage()]] as const) {
      for (const [role, name] of Object.entries(inks[mode])) {
        expect(scheme[role], `${mode}.${role}`).toBe(`var(--kit-${name})`);
        expect(hex(scheme[role]), `${mode}.${role}`).toBe(kit(name));
      }
    }
    expect(hex(light().bg)).toBe(kit("warm-paper"));
    expect(hex(light().panel)).toBe(kit("white"));
    for (const role of GROUNDS) expect(pinnedDark()[role], role).toMatch(/^var\(--ocean-[\w-]+\)$/);
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

  it("puts white text on the logo gradient only where it clears 4.5:1", () => {
    const fill = CSS.match(/--brand-fill:\s*linear-gradient\(([^;]+)\);/)?.[1] ?? "";
    const stops = [...fill.matchAll(/#[0-9a-f]{6}|var\(--brand-[\w-]+\)/gi)].map((m) => hex(m[0]));
    expect(stops.length).toBeGreaterThanOrEqual(2);
    for (const stop of stops) expect(contrast("#ffffff", stop), stop).toBeGreaterThanOrEqual(4.5);
    expect(hex("var(--brand-deep)")).toBe("#0b48bb");
    expect(hex("var(--brand-light)")).toBe("#1e8fd8");
  });

  function stops(token: string): string[] {
    const ramp = CSS.match(new RegExp(`--${token}:\\s*linear-gradient\\(([^;]+)\\);`))?.[1] ?? "";
    return [...ramp.matchAll(/#[0-9a-f]{6}|var\(--[\w-]+\)/gi)].map((m) => hex(m[0]));
  }

  it("keeps sea text readable on every stop of the sea ramps", () => {
    for (const token of ["sea", "sea-band"]) {
      const ramp = stops(token);
      expect(ramp.length, token).toBeGreaterThanOrEqual(3);
      for (const stop of ramp) {
        expect(contrast(hex(stage().muted), stop), `${token}: muted on ${stop}`).toBeGreaterThanOrEqual(4.5);
        expect(contrast(hex(stage().text), stop), `${token}: text on ${stop}`).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("keeps stage text readable over the deep tide stops", () => {
    // The tide runs paper -> aqua -> logo light -> logo deep -> mid -> navy.
    // Only its deep stops can carry stage text; the pale stops cannot.
    const ramp = stops("tide");
    const deep = ramp.indexOf(hex("var(--brand-deep)"));
    const mid = ramp.indexOf(hex("var(--sea-mid)"));
    expect(deep).toBeGreaterThan(0);
    expect(mid).toBeGreaterThan(deep);
    for (const stop of ramp.slice(deep)) {
      expect(contrast(hex(stage().text), stop), `text on ${stop}`).toBeGreaterThanOrEqual(4.5);
    }
    for (const stop of ramp.slice(mid)) {
      expect(contrast(hex(stage().muted), stop), `muted on ${stop}`).toBeGreaterThanOrEqual(4.5);
    }
  });
});
