import { existsSync, readdirSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { siteCss } from "./site-css";

// The whale-road (codewhale-design/DIRECTION.md): one character drawn from
// the v2 kit, a footer on the page's own ground, and a header that stays
// usable in every locale.
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

  it("uses the mono mark as the site identity and the colored whale only as a character", () => {
    const icon = web("app/icon.svg");
    expect(icon).toContain("prefers-color-scheme:dark");
    expect(icon).not.toContain("#1E8FD8");
    expect(icon).not.toMatch(/<rect/);
    expect(web("app/manifest.ts")).not.toContain("#142352");
    for (const file of ["components/nav.tsx", "components/footer.tsx"]) {
      expect(web(file), file).toContain("brand-mark");
      expect(web(file), file).not.toContain("WhalePose");
    }
    expect(web("app/styles/shell.css")).toContain('mask: url("/brand/mark-mono.svg")');
  });

  it("keeps the founder's 404 poster and seals", () => {
    // The Codwhale poster is the 404 joke, restored on purpose (site merge plan R4).
    expect(existsSync(new URL("../public/codwhale-404.webp", import.meta.url))).toBe(true);
    expect(web("components/route-state.tsx")).toContain('src="/codwhale-404.webp"');
    expect(existsSync(new URL("../components/seal.tsx", import.meta.url))).toBe(true);
    expect(existsSync(new URL("../components/install-binary.tsx", import.meta.url))).toBe(false);
  });
});

describe("the footer", () => {
  it("sits on the page's own ground under one hairline, with no navy band", () => {
    const footer = web("components/footer.tsx");
    expect(footer).not.toMatch(/horizon|sea-texture|site-footer-sea|Strata/);
    expect(CSS).toMatch(/\.site-footer \{[^}]*border-top: 1px solid var\(--line\);/);
    expect(CSS).not.toMatch(/\.site-footer \{[^}]*background:/);
    expect(CSS).not.toMatch(/site-footer-sea|sea-texture/);
    expect(web("app/[locale]/page.tsx")).not.toContain("sea-continues");
  });

  it("folds its four link groups into disclosures on a phone", () => {
    const footer = web("components/footer.tsx");
    expect(footer).toContain('<details key={group.label} className="site-footer-fold"');
    expect(CSS).toMatch(/@media \(max-width: 640px\) \{\s*\.site-footer-links \{ display: none; \}\s*\.site-footer-folds \{ display: block; \}/);
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
