import { existsSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { resolveWhale } from "./whale-tokens";
import { siteCss } from "./site-css";

// Radii, state roles and motion follow the GPUI client (DESIGN.md, set_theme,
// motion.rs): one radius grammar, one focus ring, spring-fitted easing, and a
// still pose under reduced motion.
const CSS = siteCss();
const TAILWIND = readFileSync(new URL("../tailwind.config.ts", import.meta.url), "utf8");

describe("design grammar contract", () => {
  it("draws every radius from the 6/10/14/pill grammar", () => {
    const defined = [...CSS.matchAll(/--radius-[\w-]+:\s*([^;]+);/g)].map((m) => resolveWhale(m[1].trim()));
    expect(new Set(defined)).toEqual(new Set(["6px", "10px", "14px", "999px"]));
    const used = [...CSS.matchAll(/border-radius:\s*([^;]+);/g)].map((m) => m[1].trim());
    expect(used.length).toBeGreaterThan(0);
    // A corner is a grammar token or square; per-corner shorthands combine them.
    for (const value of used) {
      expect(value).toMatch(/^(?:(?:var\(--radius-(control|surface|sheet|pill)\)|0)\s*){1,4}$/);
    }
    const scale = TAILWIND.match(/borderRadius:\s*\{([\s\S]*?)\}/)?.[1] ?? "";
    expect(scale).not.toMatch(/\d+(px|rem)/);
  });

  it("has one focus ring and the set_theme selection", () => {
    expect(CSS.match(/(?<![\w-])outline:/g)).toHaveLength(1);
    expect(CSS).toContain(":focus-visible { outline: var(--gpui-focus-width) solid var(--ring); outline-offset: var(--gpui-focus-offset); }");
    expect(resolveWhale("var(--gpui-focus-width)")).toBe("2px");
    expect(resolveWhale("var(--gpui-focus-offset)")).toBe("3px");
    expect(CSS).not.toMatch(/outline:\s*none/);
    expect(CSS).toMatch(/::selection \{ background: var\(--selection\); \}/);
    // The light ring on the dark stage would fall under 3:1, so the stage
    // re-declares it.
    expect(CSS).toMatch(/\.stage,\s*\.site-footer\s*\{[^}]*--ring: var\(--kit-bioluminescence\);/);
  });

  it("paints every primary fill with the logo gradient and white text", () => {
    for (const cls of ["btn-primary", "paper-install-cta"]) {
      const rest = CSS.match(new RegExp(`\\.${cls}[^{]*\\{([^}]*)\\}`))?.[1] ?? "";
      const hover = CSS.match(new RegExp(`\\.${cls}:hover[^{]*\\{([^}]*)\\}`))?.[1] ?? "";
      expect(rest, cls).toMatch(/background: var\(--brand-fill\)/);
      expect(rest, cls).toMatch(/color: var\(--on-brand\)/);
      expect(hover, cls).toMatch(/background: var\(--brand-fill-hover\)/);
    }
  });

  it("keeps every keyframed animation behind a motion preference", () => {
    // Strip the blocks that only run when motion is allowed; nothing left
    // may start an animation.
    let rest = CSS;
    for (;;) {
      const at = rest.indexOf("@media (prefers-reduced-motion: no-preference)");
      if (at < 0) break;
      let depth = 0;
      let end = rest.indexOf("{", at);
      for (let i = end; i < rest.length; i++) {
        if (rest[i] === "{") depth++;
        else if (rest[i] === "}" && --depth === 0) { end = i; break; }
      }
      rest = rest.slice(0, at) + rest.slice(end + 1);
    }
    const starts = [...rest.matchAll(/animation:\s*([^;]+);/g)].map((m) => m[1].trim());
    for (const value of starts) expect(value).toBe("none");
  });

  it("times motion with the spring tokens and stills it under reduced motion", () => {
    expect(CSS).not.toMatch(/\d+ms ease[,;]/);
    expect(CSS).not.toMatch(/cubic-bezier\(0\.25, 0\.46/);
    expect(CSS).toMatch(/--ease-spring: cubic-bezier\(/);
    expect(CSS).toMatch(
      /@media \(prefers-reduced-motion: reduce\) \{\s*:root \{\s*--dur-state: 0ms;\s*--dur-spring: 0ms;/,
    );
    expect(TAILWIND).toMatch(/transitionDuration: \{ DEFAULT: "var\(--dur-state\)" \}/);
    expect(TAILWIND).toMatch(/transitionTimingFunction: \{ DEFAULT: "var\(--ease-spring\)" \}/);
    expect(CSS).not.toMatch(/caustic/);
  });

  it("keeps no unused presence shapes", () => {
    expect(existsSync(new URL("../components/presence.tsx", import.meta.url))).toBe(false);
  });
});
