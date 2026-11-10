/**
 * Fleet is the canonical public noun; Pod remains a documented compatibility
 * alias for the CLI and TUI surfaces.
 *
 * These deterministic source contracts read the real registry, sitemap,
 * routes, and repository docs so discovery cannot silently split across nouns.
 */
import { existsSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import buildSitemap from "../app/sitemap";
import { DOC_TOPICS, docTopicHref, getTopic } from "./docs-map";
import { filterDocTopics } from "./search-utils";
import { PRODUCT_TERMS } from "./content/vocabulary";
import { GETTING_STARTED_STEPS } from "./content/getting-started";
import { buildLlmsTxt } from "./llms-txt";
import { SITE_URL } from "./page-meta";

const webRoot = new URL("../", import.meta.url);
const repoRoot = new URL("../../", import.meta.url);

function webText(path: string): string {
  return readFileSync(new URL(path, webRoot), "utf8");
}

function repoText(path: string): string {
  return readFileSync(new URL(path, repoRoot), "utf8");
}

const sitemapEntries = buildSitemap();

describe("Fleet is the canonical public surface", () => {
  it("registers exactly one roster topic at /docs/fleet", () => {
    const fleet = getTopic("fleet");
    expect(fleet?.hasPage).toBe(true);
    expect(fleet?.slug).toBe("fleet");
    // The page is task-named ("Run a workflow"); the Fleet noun lives in
    // its description, never the retired Pod noun.
    expect(fleet?.description.en).toContain("Fleet");
    expect(`${fleet?.label.en} ${fleet?.description.en}`).not.toContain("Pod");
    expect(docTopicHref(fleet!, "en")).toBe("/en/docs/fleet");
    expect(DOC_TOPICS.filter((t) => t.id === "fleet")).toHaveLength(1);
    expect(DOC_TOPICS.map((t) => t.id)).not.toContain("pod");
  });

  it("indexes /docs/fleet and keeps /docs/pod out of the sitemap", () => {
    for (const locale of ["en", "zh"]) {
      expect(
        sitemapEntries.some((e) => e.url === `${SITE_URL}/${locale}/docs/fleet`),
        locale,
      ).toBe(true);
      expect(
        sitemapEntries.some((e) => e.url === `${SITE_URL}/${locale}/docs/pod`),
        locale,
      ).toBe(false);
    }
  });

  it("serves Fleet at /docs/fleet and permanently redirects /docs/pod", () => {
    const page = webText("app/[locale]/docs/fleet/page.tsx");
    const redirect = webText("app/[locale]/docs/pod/page.tsx");
    expect(page).toContain('path: "/docs/fleet"');
    expect(page).toContain('import { buildPageMetadata } from "@/lib/page-meta"');
    expect(redirect).toContain('import { permanentRedirect } from "next/navigation"');
    expect(redirect).toContain("permanentRedirect(`/${locale}/docs/fleet`)");
    expect(redirect).not.toContain("buildPageMetadata");
    expect(existsSync(new URL("app/[locale]/docs/fleet/page.tsx", webRoot))).toBe(true);
    expect(existsSync(new URL("app/[locale]/docs/pod/page.tsx", webRoot))).toBe(true);
  });

  it("uses /docs/fleet in the machine-readable index", () => {
    const llms = buildLlmsTxt();
    expect(llms).toContain("/docs/fleet");
    expect(llms).not.toContain("/docs/pod");
    expect(llms).toContain(getTopic("fleet")!.label.en);
  });

  it("resolves docs search on both nouns to the one Fleet page", () => {
    for (const query of ["fleet", "Fleet", "pod", "Pod"]) {
      const hits = filterDocTopics(DOC_TOPICS, query).map((i) => DOC_TOPICS[i]);
      expect(hits.filter((t) => t.id === "fleet"), query).toHaveLength(1);
    }
  });

  it("uses Fleet in shared vocabulary and the guided path", () => {
    expect(PRODUCT_TERMS.map((t) => t.term)).toEqual(["Fleet", "Workflow", "Lane", "Runtime"]);
    const step = GETTING_STARTED_STEPS.find((s) => s.id === "fleet-workflow");
    expect(step).toBeTruthy();
    expect(step!.link.href).toBe("/docs/fleet");
    expect(step!.commands).toContain("/fleet setup");
    expect(step!.commands).toContain("/fleet workers");
  });

  it("keeps durable Fleet status separate from current-session workers", () => {
    const page = webText("app/[locale]/docs/fleet/page.tsx");
    expect(page).toContain("getDocsFleet");
    for (const locale of ["en", "zh"]) {
      const source = webText(`lib/i18n/dictionaries/${locale}/docs-fleet.ts`);
      // Durable run history (shell) and this session's workers (TUI) are
      // named as two different surfaces in both languages.
      expect(source, locale).toContain("codewhale fleet status");
      expect(source, locale).toContain("/fleet workers");
      expect(source, locale).toContain("/subagents");
    }
  });

  it("documents saved fleets separately from members and workers", () => {
    for (const locale of ["en", "zh"]) {
      const source = webText(`lib/i18n/dictionaries/${locale}/docs-fleet.ts`);
      expect(source, locale).toContain("/fleet saved");
      expect(source, locale).toContain("/fleet setup");
      expect(source, locale).toContain("codewhale workflow run");
      expect(source, locale).toContain("codewhale lane");
    }
  });
});

describe("Fleet compatibility boundary", () => {
  it("documents the canonical commands, aliases, and shared Fleet artifacts", () => {
    const doc = repoText("docs/FLEET.md");
    expect(doc).toContain("`codewhale fleet …`");
    expect(doc).toContain("`/fleet …`");
    // Pod was ripped out before ever shipping: no pod aliases documented.
    expect(doc).not.toContain("`/pod`");
    expect(doc).not.toContain("codewhale pod");
    for (const artifact of [
      ".codewhale/fleet.jsonl",
      "fleets/<name>.toml",
      "`[fleet]`",
      "--fleet",
      "fleet.status",
    ]) {
      expect(doc, artifact).toContain(artifact);
    }
  });

  it("pins Fleet to the exact public fact definition", () => {
    const matrix = JSON.parse(repoText("docs/public-surface-facts.json")) as {
      product: { terminology: Record<string, string> };
    };
    expect(Object.keys(matrix.product.terminology)).toContain("Fleet");
    expect(Object.keys(matrix.product.terminology)).not.toContain("Pod");
    const fleet = PRODUCT_TERMS.find((t) => t.term === "Fleet")!;
    expect(fleet.short.en).toBe(matrix.product.terminology.Fleet);
    expect(fleet.short.en).toBe(
      "the user's model inventory: who is in the roster and which member is selected",
    );
    expect(repoText("docs/FLEET.md")).toContain(`**Fleet** = ${fleet.short.en}`);
  });
});
