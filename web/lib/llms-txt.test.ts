import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import InstallPage from "../app/[locale]/install/page";
import { GET as getTopicMarkdown } from "../app/docs-markdown/[topic]/route";
import { GET as getFullText } from "../app/llms-full.txt/route";
import { GET } from "../app/llms.txt/route";
import robots from "../app/robots";
import { RELEASE_DOCS } from "./release-docs";
import { claimAvailability, runtimeCapabilityContext } from "./capabilities";
import { INSTALL_COMMANDS, INSTALL_COPY, INSTALL_PAGE_COMMANDS } from "./content/install";
import { PRODUCT_MAP_STATE_KEY, PRODUCT_MAP_TEXT_KEYS, productMap } from "./content/product-map";
import { REFERENCE_GUIDE_COPY } from "./content/reference-guides";
import type { DesktopReleaseFact } from "./desktop-release";
import { DOC_TOPICS, docTopicHref } from "./docs-map";
import type { RepoFacts } from "./facts";
import { FACTS } from "./facts.generated";
import { EN_HOME, fill } from "./i18n/dictionaries";
import { DISCORD_URL, REPO_URL } from "./i18n/links";
import { buildInstallMarkdown, buildLlmsTxt } from "./llms-txt";
import { SITE_NAME, SITE_URL } from "./page-meta";
import { REDIRECTED_ROUTES } from "./route-redirects";

const release = FACTS.latestPublishedRelease!;

const desktopRelease: DesktopReleaseFact = {
  tag: "v1.2.3",
  version: "1.2.3",
  publishedAt: "2026-12-01T10:00:00Z",
  readAt: "2026-12-01T11:00:00Z",
  url: "https://github.com/codewhale-hq/codewhale-app/releases/tag/v1.2.3",
  assets: [
    {
      name: "Codewhale-1.2.3-arm64.dmg",
      platform: "macos",
      size: 123_456_789,
      sha256: "a".repeat(64),
      signed: true,
      notarized: true,
      url: "https://github.com/codewhale-hq/codewhale-app/releases/download/v1.2.3/Codewhale-1.2.3-arm64.dmg",
    },
  ],
};

const PRICED = /[$€£¥￥]\s?\d|\b\d[\d,.]*\s?(?:USD|CNY|RMB|EUR|GBP)\b|\bper (?:month|seat|user|year)\b|\b(?:Free|Pro|Plus|Team|Teams|Max|Starter|Premium|Business|Enterprise|Individual)\s+(?:plan|tier|subscription)s?\b|\bpricing\b|\bbilled\b|\bfree trial\b|\ballowances?\b/i;
const DATED = /\b\d{4}-\d{2}-\d{2}\b|\b(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*\.? \d{1,2}\b|\b(?:19|20)\d{2}\b/;

const mapLine = (id: keyof typeof PRODUCT_MAP_TEXT_KEYS, state: keyof typeof PRODUCT_MAP_STATE_KEY) =>
  `- ${EN_HOME[PRODUCT_MAP_TEXT_KEYS[id].name]}: ${EN_HOME[PRODUCT_MAP_STATE_KEY[state]]}.`;

const withRelease = (version: string): RepoFacts => ({
  ...FACTS,
  latestPublishedRelease: { ...release, tag: `v${version}`, version, url: release.url.replace(release.tag, `v${version}`) },
});

describe("llms.txt", () => {
  it("opens with the locked headline and subhead and indexes the docs topics and the pages the sitemap keeps", () => {
    const body = buildLlmsTxt();

    expect(body.startsWith(`# ${SITE_NAME}\n\n> ${EN_HOME.heroTitle}\n\n${fill(EN_HOME.heroIntro, { brand: SITE_NAME })}\n`)).toBe(true);
    expect(body).toContain(REPO_URL);
    expect(body).toContain(DISCORD_URL);

    for (const topic of DOC_TOPICS) {
      if (topic.hasPage) {
        expect(body, topic.id).toContain(`${SITE_URL}${docTopicHref(topic, "en")}`);
        expect(body, topic.id).toContain(topic.id === "install" ? INSTALL_COPY.metaDescription.en : topic.description.en);
      } else {
        expect(body, topic.id).toContain(topic.label.en);
      }
    }

    for (const path of ["/en/install", "/en/docs/guide", "/en/models", "/en/changelog", "/en/app", "/en/faq", "/en/computer-use", "/en/contribute", "/en/docs", "/en/ratatui", "/en/legal/terms", "/en/legal/privacy"]) {
      expect(body, path).toContain(`${SITE_URL}${path}`);
    }
    const linked = [...body.matchAll(/https:\/\/codewhale\.net\/en\/([a-z-]+)/g)].map((match) => match[1]);
    expect(linked.length).toBeGreaterThan(DOC_TOPICS.length);
    for (const segment of linked) expect(REDIRECTED_ROUTES, segment).not.toContain(segment);
  });

  it("names the published release and never the source version", () => {
    const body = buildLlmsTxt();
    expect(body).toContain(`The latest release is v${release.version}.`);
    expect(body).toContain(`codewhale ${release.version}`);
    if (FACTS.version && FACTS.version !== release.version) expect(body).not.toContain(FACTS.version);

    const other = buildLlmsTxt({ facts: withRelease("9.8.7") });
    expect(other).toContain("The latest release is v9.8.7.");
    expect(other).toContain("codewhale 9.8.7");
    expect(other).not.toContain(release.version);
  });

  it("gives every install command and the same steps as the install page", async () => {
    const html = renderToStaticMarkup(await InstallPage({ params: Promise.resolve({ locale: "en" }) }));
    const page = html.replace(/<[^>]+>/g, "").replaceAll("&lt;", "<").replaceAll("&gt;", ">").replaceAll("&quot;", '"').replaceAll("&#x27;", "'").replaceAll("&amp;", "&");
    const stated = [
      ...Object.values(INSTALL_COPY).map((copy) => fill(copy.en, { version: release.version }).replaceAll("`", "")),
      ...Object.values(INSTALL_COMMANDS),
      ...Object.values(INSTALL_PAGE_COMMANDS),
    ].filter((text) => page.includes(text));
    expect(stated.length).toBeGreaterThan(Object.keys(INSTALL_COPY).length / 2);

    for (const text of [buildLlmsTxt(), buildInstallMarkdown()]) {
      const plain = text.replaceAll("`", "");
      for (const statement of stated) expect(plain, statement).toContain(statement);
      for (const command of Object.values(INSTALL_COMMANDS)) expect(text, command).toContain(`\`${command}\``);
      const uninstallReleased = claimAvailability({ feature: "unix-uninstall" }, runtimeCapabilityContext(FACTS)) === "released";
      expect(text.includes(INSTALL_PAGE_COMMANDS.uninstall)).toBe(uninstallReleased);
    }
  });

  it("states each part of the product with its computed state, and a desktop release flips only the desktop line", () => {
    const body = buildLlmsTxt({ surface: { desktopRelease: null } });
    const rows = productMap({ surface: { desktopRelease: null }, capabilities: runtimeCapabilityContext(FACTS) });
    expect(rows).toHaveLength(6);
    for (const row of rows) {
      expect(body, row.id).toContain(mapLine(row.id, row.state));
      expect(body.includes(EN_HOME[PRODUCT_MAP_TEXT_KEYS[row.id].sentence]), row.id).toBe(row.sentenceHolds);
    }
    expect(body).toContain(`${mapLine("desktop", "developing")} ${EN_HOME.mapDesktopSentence}`);

    const flipped = buildLlmsTxt({ surface: { desktopRelease } });
    expect(flipped).toContain(mapLine("desktop", "released"));
    expect(flipped).not.toContain(mapLine("desktop", "developing"));
    expect(flipped).not.toContain(EN_HOME.mapDesktopSentence);
    for (const row of rows.filter((entry) => entry.id !== "desktop")) expect(flipped, row.id).toContain(mapLine(row.id, row.state));
  });

  it("carries no price, plan, date outside the provenance stamp, or retired headline", () => {
    for (const text of [buildLlmsTxt(), buildInstallMarkdown()]) {
      const lines = text.split("\n");
      const stamp = lines.filter((line) => /^(?:- )?Facts generated /.test(line));
      expect(stamp).toHaveLength(1);
      expect(stamp[0]).toContain(FACTS.generatedAt.slice(0, 10));

      const rest = lines.filter((line) => line !== stamp[0]).join("\n");
      expect(rest.match(PRICED)?.[0]).toBeUndefined();
      expect(rest.match(DATED)?.[0]).toBeUndefined();
      expect(rest).not.toContain("agent you can read");
    }
  });

  it("says unknown, or leaves the line out, when the release or a count is unknown", () => {
    const unreadable: RepoFacts = { ...FACTS, latestPublishedRelease: null };
    const body = buildLlmsTxt({ facts: unreadable });
    expect(body).toContain("The latest release is unknown");
    expect(body).not.toContain("Built-in provider routes");
    expect(body).not.toContain(mapLine("manyAgents", "released"));

    for (const text of [body, buildInstallMarkdown({ facts: unreadable })]) {
      expect(text).toContain(INSTALL_COPY.verifyBodyUnknown.en);
      expect(text).toContain(INSTALL_COPY.removeFilesOnlyUnknown.en);
      expect(text).not.toContain(INSTALL_PAGE_COMMANDS.uninstall);
      expect(text.match(/\d+\.\d+\.\d+|\{\w+\}|undefined|\bnull\b|NaN/)?.[0]).toBeUndefined();
    }

    const untagged = buildLlmsTxt({ facts: withRelease("9.8.7") });
    expect(untagged).not.toContain("Built-in provider routes");
    expect(untagged).not.toMatch(/\(0\)/);
  });

  it("serves the generated body as text/plain from the well-known route", async () => {
    const response = GET();
    expect(response.headers.get("content-type")).toBe("text/plain; charset=utf-8");
    expect(await response.text()).toBe(buildLlmsTxt());
  });

  it("serves the install steps as markdown, alone and inside the full text", async () => {
    const install = await getTopicMarkdown(new Request(`${SITE_URL}/docs-markdown/install.md`), { params: Promise.resolve({ topic: "install.md" }) });
    expect(install.headers.get("content-type")).toBe("text/markdown; charset=utf-8");
    expect(await install.text()).toBe(buildInstallMarkdown());

    const full = await getFullText().text();
    expect(full.startsWith(buildLlmsTxt())).toBe(true);
    expect(full).toContain(buildInstallMarkdown());
  });

  it("serves the documents as they were at the published release tag", async () => {
    const full = await getFullText().text();
    const label = ` at the published release ${release.tag}.`;
    expect(RELEASE_DOCS.tag).toBe(release.tag);
    expect(full.split(label).length - 1).toBe(RELEASE_DOCS.topics.length - 1);
    expect(full).not.toContain(REFERENCE_GUIDE_COPY.scope.en.slice(0, 40));
    expect(full).not.toContain(`Source: ${REPO_URL}/blob/main/`);
  });

  it("advertises /llms.txt from robots without inventing a robots field", () => {
    const spec = robots();
    expect(spec.rules).toEqual([
      {
        userAgent: "*",
        allow: ["/", "/llms.txt", "/llms-full.txt", "/features.json", "/docs-markdown/"],
        disallow: ["/api/", "/*/admin"],
      },
      {
        userAgent: ["GPTBot", "OAI-SearchBot", "ChatGPT-User", "ClaudeBot", "Claude-SearchBot", "Claude-User", "PerplexityBot", "Perplexity-User", "Google-Extended"],
        allow: "/",
        disallow: ["/api/", "/*/admin"],
      },
    ]);
    expect(spec.sitemap).toBe(`${SITE_URL}/sitemap.xml`);

    const route = readFileSync(new URL("../app/llms.txt/route.ts", import.meta.url), "utf8");
    expect(route).toContain("buildLlmsTxt()");
  });
});
