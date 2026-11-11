import { readFileSync } from "node:fs";
import { buildInstallGuide } from "../scripts/install-guide-lib.mjs";
import { FACTS } from "./facts.generated";
import { CONSTITUTION_SCENES } from "./content/constitution";
import { describe, expect, it } from "vitest";
import { getDocSearchSections } from "./docs-article-search";
import { searchDocSections } from "./doc-search-match";

describe("article section search", () => {
  it("finds commands and body-only text at the rendered section in English and Chinese", () => {
    expect(searchDocSections(getDocSearchSections("en"), "codewhale auth clear"))
      .toEqual(expect.arrayContaining([expect.objectContaining({ href: "/docs/auth#which-key", excerpt: expect.stringContaining("codewhale auth clear") })]));
    expect(searchDocSections(getDocSearchSections("zh"), "--api-key-stdin"))
      .toEqual(expect.arrayContaining([expect.objectContaining({ href: "/docs/auth#save-key" })]));
    expect(searchDocSections(getDocSearchSections("en"), "Read one source file"))
      .toEqual(expect.arrayContaining([expect.objectContaining({ href: "/docs/guide#first-session" })]));
    expect(searchDocSections(getDocSearchSections("en"), "  ")).toEqual([]);
    expect(searchDocSections(getDocSearchSections("en"), "no-such-command-12345")).toEqual([]);
    expect(getDocSearchSections("en").map((section) => section.text).join(" ")).not.toContain("Source documents: docs/INSTALL.md §8, docs/PROVIDERS.md, docs/CODEWHALE_AGENT.md");
  });

  it("finds body-only content on every custom documentation destination at existing anchors", () => {
    for (const locale of ["en", "zh"] as const) {
      const sections = getDocSearchSections(locale);
      const searches = [
        ["winget install --id", "/install#install-overview"],
        ["codewhale --version", "/install#install-verify"],
        ["DEEPSEEK_API_KEY", "/models#models-setup"],
        ["/plugin trust <name>", "/plugins#safe-title"],
        ["extension_host", "/plugins#reference-title"],
        [".codewhale/constitution.json", "/constitution#constitution-rank"],
        [CONSTITUTION_SCENES[2].trace, "/constitution#constitution-trace"],
      ];
      for (const [query, href] of searches) {
        expect(searchDocSections(sections, query), `${locale}: ${query}`)
          .toEqual(expect.arrayContaining([expect.objectContaining({ href })]));
      }
      for (const model of FACTS.models) {
        expect(searchDocSections(sections, model.id)).toEqual(expect.arrayContaining([expect.objectContaining({ href: "/models#models-title" })]));
      }
      for (const provider of FACTS.providers) {
        expect(searchDocSections(sections, provider.env)).toEqual(expect.arrayContaining([expect.objectContaining({ href: "/models#providers-title" })]));
      }
      for (const path of ["install", "models", "plugins", "constitution"]) {
        const source = readFileSync(new URL(`../app/[locale]/${path === "constitution" ? "docs/constitution" : path}/page.tsx`, import.meta.url), "utf8");
        const ids = new Set([...source.matchAll(/(?:id|titleId)="([^"]+)"/g)].map(match => match[1]));
        const pageSections = sections.filter(section => section.href.startsWith(`/${path}#`));
        expect(pageSections.length).toBeGreaterThan(0);
        for (const section of pageSections) expect(ids.has(section.href.split("#")[1]), section.href).toBe(true);
      }
    }
  });

  it("derives install search sections from the same visible Markdown and heading ids", () => {
    const guide = buildInstallGuide('# Setup\n\n## Notes\n\n1. Run this:\n\n   ```sh\n   echo nested\n   ```\n\n| Need | Reason |\n| --- | --- |\n| curl | Download |\n\n[Visible label](https://example.com/hidden-url)\n\n## Notes\n\nSecond body.\n');
    expect(guide.searchSections.map((section: { id: string }) => section.id)).toEqual(["setup", "notes", "notes-1"]);
    expect(guide.searchSections[1].text).toContain("echo nested");
    expect(guide.searchSections[1].text).toContain("curl Download");
    expect(guide.searchSections[1].text).toContain("Visible label");
    expect(guide.searchSections[1].text).not.toContain("hidden-url");
    expect(guide.searchSections[2].text).toContain("Second body.");
    expect(buildInstallGuide("Visible introduction.\n\n## Section\n\nBody.").preamble).toBe("Visible introduction.");
  });

});
