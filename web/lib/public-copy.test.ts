import { existsSync, readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { FACTS } from "./facts.generated";
import { RELEASE_CONTRIBUTORS, RELEASE_HELPERS } from "./release-credits";
import { EN_CHROME, EN_DOCS_SHELL, EN_HOME, getChrome } from "./i18n/dictionaries";

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
    const footer = readFileSync(new URL("../components/footer.tsx", import.meta.url), "utf8");

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
    const runtime = readFileSync(new URL("./content/runtime-integrations.ts", import.meta.url), "utf8");
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

    expect(homepage).toContain("facts.latestPublishedRelease");
    // The home names only the published release; the source version never
    // reaches it.
    expect(homepage).toContain("fill(d.installNote, { version: release.version })");
    expect(homepage).not.toContain("facts.version");
    expect(EN_HOME.installNote).toBe("Version {version} is released, and it needs no account.");
    expect(readFileSync(new URL("../components/release-updates.tsx", import.meta.url), "utf8")).toContain("href={release.url}");
    // A development-source route count is not a released-provider total.
    expect(homepage).not.toContain("<span>{providerRoutes}</span>");
    expect(homepage).not.toContain("releases/tag/v${version}");
    expect(homepage).not.toMatch(/Codewhale v0\.9\.1|\"v0\.9\.1 \u00b7/);
    expect(install).toContain("facts.latestPublishedRelease");
    expect(install).not.toContain("facts.version");
    expect(install).not.toContain('"v0.8.x"');
    expect(install).not.toContain("cnbInstall(facts.version");
  });

  it("presents providers as peers", () => {
    const providerCopy = `${pageSource("models/page.tsx")}\n${pageSource("faq/page.tsx")}`;

    expect(providerCopy).not.toMatch(/first-class|一级支持|一级模型/);
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
