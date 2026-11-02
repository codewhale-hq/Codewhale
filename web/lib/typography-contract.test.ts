import { readdirSync, readFileSync, statSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { resolveWhale } from "./whale-tokens";
import { siteCss } from "./site-css";

// The site's type follows GPUI set_theme: Shannon Sans for every role, the
// system monospace stack at 13px for code, no serif and no all-caps labels.
const CSS = siteCss();
const LAYOUT = readFileSync(new URL("../app/[locale]/layout.tsx", import.meta.url), "utf8");
const TAILWIND = readFileSync(new URL("../tailwind.config.ts", import.meta.url), "utf8");
const FONTS = new URL("../public/brand/fonts/", import.meta.url);

function rootVar(name: string): string {
  const match = CSS.match(new RegExp(`--${name}:\\s*([^;]+);`));
  if (!match) throw new Error(`Missing --${name}`);
  return match[1].replace(/\s+/g, " ");
}

describe("typography contract", () => {
  it("loads no serif or webfont mono and sets no all-caps", () => {
    expect(CSS).not.toMatch(/Newsreader|IBM Plex|JetBrains|--font-serif/);
    expect(CSS).not.toMatch(/text-transform:\s*uppercase/);
    expect(LAYOUT).not.toMatch(/next\/font\/google/);
    expect(TAILWIND).not.toMatch(/Newsreader|JetBrains|widest|wider/);
    expect(TAILWIND).toMatch(/textTransform:\s*false/);
  });

  it("leaves no caller on the ungenerated all-caps or wide-tracking utilities", () => {
    // Tailwind no longer emits these, so a surviving class is a silent no-op
    // that reads as intent. Labels are sentence case at normal tracking.
    const offenders: string[] = [];
    for (const dir of ["../app/", "../components/"]) {
      for (const entry of readdirSync(new URL(dir, import.meta.url), { recursive: true })) {
        const path = String(entry);
        if (!path.endsWith(".tsx")) continue;
        const source = readFileSync(new URL(dir + path, import.meta.url), "utf8");
        if (/\b(uppercase|tracking-wider|tracking-widest)\b/.test(source)) offenders.push(dir + path);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("resolves every role to Shannon Sans and code to the system mono stack", () => {
    expect(rootVar("font-body")).toMatch(/^var\(--font-shannon-latin\), var\(--font-shannon-ext\),/);
    expect(rootVar("font-display")).toBe("var(--font-body)");
    expect(rootVar("font-mono")).toMatch(/^ui-monospace,/);
    expect(rootVar("font-cjk")).not.toMatch(/Serif|(?<!sans-)serif/);
    expect(resolveWhale(rootVar("text-mono"))).toBe("0.8125rem");
    expect(resolveWhale(rootVar("text-prose"))).toBe("1rem");
  });

  it("preloads only the Latin subset of Shannon Sans", () => {
    const faces = [...LAYOUT.matchAll(/localFont\(\{([\s\S]*?)\n\}\);/g)].map((m) => m[1]);
    expect(faces).toHaveLength(2);
    const [latin, ext] = faces;
    expect(latin).toContain("ShannonSans-Variable-latin.woff2");
    expect(latin).not.toContain("preload: false");
    expect(ext).toContain("ShannonSans-Variable-ext.woff2");
    expect(ext).toContain("preload: false");
    for (const face of faces) expect(face).toContain('prop: "unicode-range"');
    // The Latin face is the only font on the critical path; keep it small.
    expect(statSync(new URL("ShannonSans-Variable-latin.woff2", FONTS)).size).toBeLessThan(80_000);
    expect(statSync(new URL("ShannonSans-Variable-ext.woff2", FONTS)).size).toBeGreaterThan(0);
  });
});
