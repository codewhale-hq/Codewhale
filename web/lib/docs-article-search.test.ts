import { describe, expect, it } from "vitest";
import { getDocSearchSections, searchDocSections } from "./docs-article-search";

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
});
