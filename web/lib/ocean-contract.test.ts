import { existsSync, readdirSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { siteCss } from "./site-css";

// The whale-road (codewhale-design/DIRECTION.md): one character drawn from
// the v2 kit, one horizon, the sea below it, and a header that stays usable
// in every locale.
const CSS = siteCss();
const web = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const POSES = [
  "rest", "listen", "think", "busy", "read", "search", "write", "run", "browse",
  "talk", "pod", "needs", "done", "hmm", "sleep", "computer", "connect",
];

describe("the whale", () => {
  it("ships all seventeen v2 poses in the logo gradient", () => {
    const dir = new URL("../public/whale/", import.meta.url);
    expect(readdirSync(dir).filter((f) => f.endsWith(".svg")).sort()).toEqual(POSES.map((p) => `${p}.svg`).sort());
    for (const pose of POSES) {
      const svg = readFileSync(new URL(`${pose}.svg`, dir), "utf8");
      expect(svg, pose).toContain('stop-color="#1E8FD8"');
      expect(svg, pose).toContain('stop-color="#0B48BB"');
      // A still pose: never animated.
      expect(svg, pose).not.toMatch(/<animate|@keyframes/);
    }
    expect(web("components/whale-pose.tsx")).toContain(POSES.map((p) => `"${p}"`).join(",\n  "));
  });

  it("uses the brand mark for the site icons, not a tile", () => {
    const icon = web("app/icon.svg");
    expect(icon).toContain('stop-color="#1E8FD8"');
    expect(icon).not.toMatch(/<rect/);
    expect(web("app/manifest.ts")).not.toContain("#142352");
  });

  it("keeps the founder's 404 poster and the restored seals and water, and no retired illustrations", () => {
    // The Codwhale poster is the 404 joke, restored on purpose (site merge plan R4).
    expect(existsSync(new URL("../public/codwhale-404.webp", import.meta.url))).toBe(true);
    expect(web("components/route-state.tsx")).toContain('src="/codwhale-404.webp"');
    // Seals stay. The blurred strata bands were retired on 2026-10-04 for
    // paper above, sea below: one horizon and the tide ombré, no wave art.
    expect(existsSync(new URL("../components/seal.tsx", import.meta.url))).toBe(true);
    expect(existsSync(new URL("../components/strata.tsx", import.meta.url))).toBe(false);
    expect(existsSync(new URL("../components/install-binary.tsx", import.meta.url))).toBe(false);
  });
});

describe("the horizon", () => {
  it("ends every page on one horizon line, then the sea", () => {
    const footer = web("components/footer.tsx");
    expect(footer).toContain('className="horizon"');
    expect(footer).not.toContain("Strata");
    expect(CSS).toMatch(/\.site-footer-sea \{[^}]*background: var\(--sea\)/);
    // A page already ending in the sea (home) runs on without a second line.
    expect(CSS).toMatch(/main:has\(\.sea-continues:last-child\) \+ \.site-footer \.site-footer-sea > \.horizon \{ display: none; \}/);
    const home = web("app/[locale]/page.tsx");
    expect(home).toContain("sea-continues");
  });

  it("paints the stage with the dark set in both appearances", () => {
    expect(CSS).toMatch(/\.stage,\s*\.site-footer\s*\{[^}]*--ring: var\(--kit-bioluminescence\);[^}]*color-scheme: dark;/);
  });

  it("keeps the horizon visible in forced colours", () => {
    expect(CSS).toMatch(/@media \(forced-colors: active\) \{\s*\.horizon \{ background: CanvasText; \}/);
  });
});

describe("the header", () => {
  it("keeps localized navigation controls inside compact viewports", () => {
    expect(CSS).toMatch(/@media \(max-width: 520px\) \{\s*\.site-nav-inner\s*\{\s*gap:\s*0\.5rem;\s*\}\s*\.site-nav-actions\s*\{\s*min-width:\s*0;/);
    expect(CSS).toMatch(/\.site-nav-actions\s*\{[\s\S]*?flex-shrink:\s*0/);
    expect(CSS).toMatch(/\.site-nav-actions\s*>\s*\*\s*\{\s*flex-shrink:\s*0/);
    expect(CSS).toMatch(/@media \(max-width: 900px\)[\s\S]*?\.site-github-link\s*\{\s*display:\s*none/);
    // The locale <select> sits inside the fixed icon box instead of sizing
    // the row, and the wordmark keeps a clickable floor.
    expect(CSS).not.toMatch(/\.site-nav-actions select/);
    expect(CSS).toMatch(/\.nav-locale select\s*\{[^}]*position:\s*absolute;[^}]*inset:\s*0;/);
    expect(CSS).toMatch(/\.paper-wordmark\s*\{[^}]*flex:\s*0 1 auto;[^}]*min-width:\s*8\.75rem;/);
  });

  it("re-inks the wordmark through the mark ink, never a filter", () => {
    expect(CSS).toMatch(/\.paper-wordmark-logo,\s*\.wordmark\s*\{[^}]*background: var\(--mark-ink\);[^}]*mask: url\("\/brand\/wordmark\.svg"\)/);
    expect(CSS).not.toMatch(/paper-wordmark-logo\s*\{\s*filter:/);
  });
});
