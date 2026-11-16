import { describe, expect, it } from "vitest";
import { RELEASE_BASELINE, type CapabilityContext } from "./capabilities";
import { toolRowsFor } from "./content/tools";
import { controlTermsFor } from "./content/vocabulary";
import { getDocsAccount } from "./content/docs-account";
import { resolveDocsPage } from "./docs-release";
import { FEATURE_CATALOGUE, type Feature } from "./features";
import {
  getDocsAuth, getDocsClaudeCode, getDocsConfiguration, getDocsFleet, getDocsHooks, getDocsMcp, getDocsModes, getDocsPet,
  getDocsReview, getDocsRuntimeApi, getDocsSandbox, getDocsSkills, getDocsSubagents, getDocsTroubleshooting, getDocsTrust,
  getDocsWeb, getDocsWork, getDocsFirstTime,
} from "./i18n/dictionaries";
import type { DocsBlock, DocsPageDict } from "./i18n/dictionaries/types";
import { DOC_TOPICS } from "./docs-map";

if (!RELEASE_BASELINE) throw new Error("release baseline missing");
const baseline = RELEASE_BASELINE;

const publishedV0102: CapabilityContext = { release: { tag: baseline.tag, version: baseline.version }, baseline };

const raisedFeatures = FEATURE_CATALOGUE.features.map((row) => (row.since === "unreleased" ? { ...row, since: "99.0.0" } : row));

const raised: CapabilityContext = {
  release: { tag: "v99.0.0", version: "99.0.0" },
  baseline: {
    ...baseline,
    tag: "v99.0.0",
    version: "99.0.0",
    features: raisedFeatures.map((row) => ({ id: row.id, name: row.name, summary: row.summary, status: row.status, since: row.since })),
  },
};

function blockText(block: DocsBlock): string {
  if ("p" in block) return block.p;
  if ("code" in block) return block.code;
  if ("rows" in block) return block.rows.flat().join(" ");
  if ("steps" in block) return block.steps.join(" ");
  if ("list" in block) return block.list.join(" ");
  return block.note;
}

const bodyText = (page: DocsPageDict, context: CapabilityContext, features: readonly Feature[] = FEATURE_CATALOGUE.features) =>
  resolveDocsPage(page, context, features).sections.map((section) => `${section.title} ${section.blocks.map(blockText).join(" ")}`).join(" ");

const sourceText = (page: DocsPageDict, context: CapabilityContext, features: readonly Feature[] = FEATURE_CATALOGUE.features) =>
  resolveDocsPage(page, context, features).sourceSections.map((section) => `${section.title} ${section.blocks.map(blockText).join(" ")}`).join(" ");

const PAGES: Record<string, (locale: string) => DocsPageDict> = {
  account: getDocsAccount, auth: getDocsAuth, "claude-code": getDocsClaudeCode, configuration: getDocsConfiguration,
  fleet: getDocsFleet, hooks: getDocsHooks, mcp: getDocsMcp, modes: getDocsModes, pet: getDocsPet, review: getDocsReview,
  "runtime-api": getDocsRuntimeApi, sandbox: getDocsSandbox, skills: getDocsSkills, subagents: getDocsSubagents,
  troubleshooting: getDocsTroubleshooting, trust: getDocsTrust, web: getDocsWeb, work: getDocsWork,
  "first-time": getDocsFirstTime,
};

describe("docs describe the published release first", () => {
  it("keeps the permission dial and Accept Edits out of every docs body while the published release is v0.10.2", () => {
    for (const [slug, getPage] of Object.entries(PAGES)) {
      for (const locale of ["en", "zh"]) {
        const text = bodyText(getPage(locale), publishedV0102);
        expect(text, `${locale}/${slug}`).not.toMatch(/Accept Edits/);
        expect(text, `${locale}/${slug}`).not.toMatch(/Ask → Accept Edits/);
      }
    }
    const vocabulary = controlTermsFor(publishedV0102).body.map((row) => row.term);
    expect(vocabulary).toEqual(["Plan", "Work", "Operate", "Ask", "Auto-Review", "Full Access"]);
  });

  it("shows the in-source modes and dial only in the labelled section, and moves them into the body when a release carries them", () => {
    const modes = getDocsModes("en");
    expect(sourceText(modes, publishedV0102)).toMatch(/Accept Edits/);
    expect(bodyText(modes, publishedV0102)).toMatch(/Plan → Work → Operate/);
    expect(bodyText(modes, publishedV0102)).toMatch(/Ask → Auto-Review → Full Access/);

    expect(sourceText(modes, raised, raisedFeatures)).toBe("");
    expect(bodyText(modes, raised, raisedFeatures)).toMatch(/Ask → Accept Edits → Plan → Auto-Review → Full Access/);
    expect(bodyText(modes, raised, raisedFeatures)).not.toMatch(/Plan → Work → Operate/);

    const vocabulary = controlTermsFor(raised, raisedFeatures);
    expect(vocabulary.source).toEqual([]);
    expect(vocabulary.body.map((row) => row.term)).toEqual(["Plan", "Work", "Ask", "Accept Edits", "Auto-Review", "Full Access"]);
  });

  it("lists source-only tools in the labelled set and moves them into the list when released", () => {
    const published = toolRowsFor(publishedV0102);
    expect(published.body.map((row) => row.name)).not.toContain("present");
    expect(published.source.map((row) => row.name)).toEqual(expect.arrayContaining(["present", "grep_files"]));
    const next = toolRowsFor(raised, raisedFeatures);
    expect(next.source).toEqual([]);
    expect(next.body.map((row) => row.name)).toEqual(expect.arrayContaining(["present", "grep_files"]));
    expect(next.body.map((row) => row.name)).not.toContain("workflow");
  });

  it("shows no source section with no published release", () => {
    const none: CapabilityContext = { release: null, baseline };
    expect(resolveDocsPage(getDocsModes("en"), none).sourceSections).toEqual([]);
  });

  it("names no other product in the claude-code page title, description, headings or topic label", () => {
    const other = /claude|codex|cursor|copilot|gemini|kimi|antigravity|alternative/i;
    for (const locale of ["en", "zh"]) {
      const page = getDocsClaudeCode(locale);
      expect(page.metaTitle).not.toMatch(other);
      expect(page.metaDescription).not.toMatch(other);
      expect(page.title).not.toMatch(other);
      for (const section of page.sections) expect(section.title).not.toMatch(other);
    }
    const topic = DOC_TOPICS.find((entry) => entry.slug === "claude-code");
    expect(topic?.label.en).not.toMatch(other);
    expect(topic?.description.en).not.toMatch(other);
  });
});
