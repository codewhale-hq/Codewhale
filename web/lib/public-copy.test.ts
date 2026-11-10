import { existsSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { FACTS } from "./facts.generated";
import { RELEASE_CONTRIBUTORS, RELEASE_HELPERS } from "./release-credits";
import { EN_CHROME, EN_COMMUNITY, EN_DOCS_SHELL, EN_HOME, getChrome } from "./i18n/dictionaries";

function pageSource(path: string): string {
  return readFileSync(new URL(`../app/[locale]/${path}`, import.meta.url), "utf8");
}

describe("public website copy contracts", () => {
  it("keeps the docs hub on the compact ocean portal instead of the old almanac treatment", () => {
    const layout = pageSource("docs/layout.tsx");
    const search = readFileSync(new URL("../components/docs-search.tsx", import.meta.url), "utf8");

    expect(layout).not.toContain("portal-mark");
    // The hero copy is dictionary-driven now (#5337), so assert it where the
    // string actually lives rather than in the TSX.
    expect(EN_DOCS_SHELL.heroTitle).toBe("Install and use Codewhale");
    expect(layout).not.toContain("Section 02");
    expect(layout).not.toContain("How Codewhale works: ego");
    expect(layout).not.toContain("<Seal");
    expect(layout.indexOf('<article className="docs-content')).toBeLessThan(
      layout.indexOf("<DocsSidebar"),
    );
    expect(search).toContain("docs-topic-row");
    expect(search).not.toContain("40+ Markdown documents");
  });

  it("keeps unreleased managed-product surfaces out of public copy", () => {
    const roadmap = pageSource("roadmap/page.tsx");
    const footer = readFileSync(new URL("../components/footer.tsx", import.meta.url), "utf8");

    expect(roadmap).toContain("Required account for the local runtime");
    expect(roadmap).not.toContain("Managed app preview");
    expect(roadmap).not.toContain("Hosted SaaS dashboard");
    expect(roadmap).not.toContain("Required login / accounts");
    expect(footer).not.toContain("App preview");
    expect(footer).not.toContain("app.codewhale.net");

    // Account entry is a real, working flow (sign-in and registration), so
    // the chrome may name it. It must never call the app a "preview" or bake
    // the app host into copy — the links own the destination.
    for (const locale of [
      "en", "zh", "ja", "vi", "ko", "ru", "uk", "es", "pt-BR", "id",
      "fr", "de", "ca", "hi", "tr", "it", "pl", "ar",
    ]) {
      const values = Object.values(getChrome(locale)).join("\n");
      expect(values, `${locale} chrome`).not.toContain("app.codewhale.net");
      expect(values, `${locale} chrome`).not.toMatch(/App preview/);
    }
    expect(EN_CHROME.footerLicense).toBe("MIT license");
  });

  it("describes ACP and the VS Code extension at their implemented capability level", () => {
    const runtime = readFileSync(new URL("./content/runtime-integrations.ts", import.meta.url), "utf8") + pageSource("runtime/page.tsx");
    const sourceDocTargets = [
      ...new Set(
        [...runtime.matchAll(/REPO_BLOB_BASE}\/([^`]+)`/g)].map((match) => match[1]),
      ),
    ];

    expect(runtime).toContain("ACP (Agent Client Protocol)");
    expect(runtime).toContain("ACP stdio connects compatible editors to the existing Engine");
    expect(runtime).toContain("Early companion for the local runtime");
    expect(runtime).not.toContain("Agent Communication Protocol");
    expect(runtime).not.toContain("IETF-standard");
    expect(runtime).not.toContain("embeds Codewhale as a side-panel agent");
    expect(runtime).not.toMatch(/\/(?:en|zh)\/docs#(?:runtime-api|acp|mcp)/);
    expect(runtime).toContain("docs/RUNTIME_API.md");
    expect(runtime).toContain("docs/MCP.md");
    expect(sourceDocTargets).toEqual(["docs/RUNTIME_API.md", "docs/MCP.md"]);
    for (const target of sourceDocTargets) {
      expect(existsSync(new URL(`../../${target}`, import.meta.url)), target).toBe(true);
    }
  });

  it("keeps source-candidate facts separate from published install facts", () => {
    const homepage = pageSource("page.tsx");
    const install = pageSource("install/page.tsx");
    const community = pageSource("community/page.tsx");

    expect(homepage).toContain("facts.latestPublishedRelease");
    // The machine-readable source-state attribute stays a literal.
    expect(homepage).toContain('data-source-state={sourceIsPublished ? "published release" : "source candidate"}');
    expect(homepage).toContain("getReleaseUpdates(facts.latestPublishedRelease)");
    expect(homepage).toContain("publishedRelease.version");
    expect(readFileSync(new URL("../components/release-updates.tsx", import.meta.url), "utf8")).toContain("href={release.url}");
    // The visible wording moved into the dictionary layer (#4934). Assert the
    // rendered contract — the EN reference value plus the page's use of it —
    // instead of a raw TSX string, and hold every locale to a real value.
    expect(homepage).toContain("d.sourceCandidate");
    expect(EN_HOME.sourceCandidate).toBe("Unreleased");
    // A development-source route count is not a released-provider total.
    expect(homepage).not.toContain("<span>{providerRoutes}</span>");
    expect(homepage).not.toContain("releases/tag/v${version}");
    expect(homepage).not.toMatch(/Codewhale v0\.9\.1|\"v0\.9\.1 \u00b7/);
    // Installation examples come from the verified guide, never the candidate version.
    expect(install).toContain('from "@/lib/install-guide.generated"');
    expect(install).not.toContain("facts.version");
    expect(install).not.toContain('"v0.8.x"');
    expect(install).not.toContain("cnbInstall(facts.version");
    // The community credit wording moved into its dictionary (#5337) the same
    // way: the page picks the unreleased label, and the EN reference says so.
    expect(community).toContain("t.creditLabelUnreleased");
    expect(EN_COMMUNITY.creditLabelUnreleased).toBe("v{version} credit (unreleased)");
  });

  it("presents providers as peers and puts contributor actions near the top", () => {
    const providerCopy = `${pageSource("models/page.tsx")}\n${pageSource("faq/page.tsx")}`;
    const community = pageSource("community/page.tsx");
    // The page's own wording lives in its dictionary (#5337); the paths and
    // links stay in the page.
    const communityCopy = `${community}\n${Object.values(EN_COMMUNITY).join("\n")}`;

    expect(providerCopy).not.toMatch(/first-class|一级支持|一级模型/);
    expect(community).toContain("t.kicker");
    expect(EN_COMMUNITY.kicker).toBe("International open-source community");
    expect(community).toContain("issues/new/choose");
    expect(community).toContain("docs/LOCALIZATION.md");
    expect(community).toContain("codewhale-hq/CodeWhale/pulls");
    expect(community).toContain("t.recordScope");
    expect(EN_COMMUNITY.recordScope).toContain("keeps the weekly archive of repository activity");
    expect(communityCopy).not.toContain("latest one sits near the top");
    expect(community).not.toContain("<Ticker");
    expect(community).not.toContain("<StatGrid");
    expect(communityCopy).not.toContain("Today's dispatch");
  });

  it("keeps current-release website credits in exact changelog parity", () => {
    expect(FACTS.version).toBeTruthy();
    const changelog = readFileSync(new URL("../../CHANGELOG.md", import.meta.url), "utf8");
    const release = changelog
      .split(`## [${FACTS.version}]`)[1]
      ?.split("\n## ")[0];
    const contributorSection = release
      ?.split("### Contributors")[1]
      ?.split("\n### ")[0];
    expect(contributorSection, `missing ${FACTS.version} contributor ledger`).toBeTruthy();

    const changelogHandles = [
      ...new Set(contributorSection?.match(/@[A-Za-z0-9_-]+/g) ?? []),
    ].sort();
    const websiteHandles = [...RELEASE_CONTRIBUTORS, ...RELEASE_HELPERS];
    const contributorDoc = readFileSync(
      new URL("../../docs/CONTRIBUTORS.md", import.meta.url),
      "utf8",
    );
    const currentDocBand = contributorDoc
      .split(`<summary><strong>v${FACTS.version} `)[1]
      ?.split("</details>")[0];
    expect(currentDocBand, `missing ${FACTS.version} contributor-doc band`).toBeTruthy();
    const docHandles = [
      ...new Set(
        [...(currentDocBand?.matchAll(/github\.com\/([A-Za-z0-9_-]+)\)/g) ?? [])].map(
          (match) => `@${match[1]}`,
        ),
      ),
    ].sort();

    expect(new Set(websiteHandles).size, "credit arrays must not overlap or repeat").toBe(
      websiteHandles.length,
    );
    expect([...websiteHandles].sort()).toEqual(changelogHandles);
    expect(docHandles).toEqual(changelogHandles);
  });
});
