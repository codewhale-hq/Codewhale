import { describe, expect, it } from "vitest";
import { GET as getTopicMarkdown, generateStaticParams } from "../app/docs-markdown/[topic]/route";
import { GET as getFullText } from "../app/llms-full.txt/route";
import { checkReleaseDocs, refreshReleaseDocs, releaseDocsIndex } from "../scripts/release-docs-lib.mjs";
import { RELEASE_DOCS, buildTopicMarkdown } from "./release-docs";
import { DOC_TOPICS } from "./docs-map";
import { FACTS } from "./facts.generated";
import { REPO_URL } from "./i18n/links";

const release = FACTS.latestPublishedRelease!;
const UNRELEASED = ["0.10.3", "codewhale uninstall", "update.mode", "Accept Edits"];

describe("documents at the released tag", () => {
  it("is a snapshot of the released version and nothing else", () => {
    expect(RELEASE_DOCS.tag).toBe(release.tag);
    expect(RELEASE_DOCS.version).toBe(release.version);
  });

  it("serves the full text and every topic without the markers of unreleased work", async () => {
    const full = await getFullText().text();
    const topics = generateStaticParams().map(({ topic }) => topic);
    expect(topics.length).toBe(RELEASE_DOCS.topics.length);
    expect(full.length).toBeGreaterThan(1_000_000);
    for (const marker of UNRELEASED) expect(full, marker).not.toContain(marker);
    for (const topic of topics) {
      const response = await getTopicMarkdown(new Request(`https://example.test/docs-markdown/${topic}`), { params: Promise.resolve({ topic }) });
      const body = await response.text();
      for (const marker of UNRELEASED) expect(body, `${topic} ${marker}`).not.toContain(marker);
    }
  });

  it("links each source at the tag and never at main", () => {
    const body = buildTopicMarkdown("mcp")!;
    expect(body).toContain(`Source: ${REPO_URL}/blob/${release.tag}/docs/MCP.md`);
    expect(body).not.toContain("Source: " + REPO_URL + "/blob/main/");
  });

  it("says plainly when a registered document is not part of the release", () => {
    const topic = RELEASE_DOCS.topics.find((entry) => entry.missing.length > 0)!;
    expect(topic).toBeDefined();
    expect(buildTopicMarkdown(topic.slug)).toContain(`${topic.missing[0]} was added after ${release.tag} and is not part of this release`);
  });

  it("answers 404 for a topic that is not in the snapshot", async () => {
    const response = await getTopicMarkdown(new Request("https://example.test/docs-markdown/nope.md"), { params: Promise.resolve({ topic: "nope.md" }) });
    expect(response.status).toBe(404);
  });

  it("fails its check on a tag mismatch and passes on the released tag", () => {
    const snapshot = JSON.parse(JSON.stringify(RELEASE_DOCS));
    expect(checkReleaseDocs(snapshot, release, DOC_TOPICS, null)).toEqual([]);
    const next = { tag: "v0.10.3", version: "0.10.3" };
    const problems = checkReleaseDocs(snapshot, next, DOC_TOPICS, null);
    expect(problems.some((line) => line.includes(`is for ${release.tag}`) && line.includes("v0.10.3"))).toBe(true);
    expect(checkReleaseDocs({ ...snapshot, tag: "v0.10.3" }, release, DOC_TOPICS, null).length).toBeGreaterThan(0);
    expect(checkReleaseDocs(null, release, DOC_TOPICS, null).length).toBeGreaterThan(0);
  });

  it("fails its check when a document is altered or a topic is dropped", () => {
    const altered = JSON.parse(JSON.stringify(RELEASE_DOCS));
    altered.topics[0].sources[0].content += "\nedited";
    expect(checkReleaseDocs(altered, release, DOC_TOPICS, null).join("\n")).toContain("hash");
    const dropped = JSON.parse(JSON.stringify(RELEASE_DOCS));
    dropped.topics.pop();
    expect(checkReleaseDocs(dropped, release, DOC_TOPICS, null).join("\n")).toContain("neither in the snapshot");
  });

  it("fails its check when the topic index disagrees with the snapshot", () => {
    expect(checkReleaseDocs(RELEASE_DOCS, release, DOC_TOPICS, null, releaseDocsIndex(RELEASE_DOCS))).toEqual([]);
    const stale = { ...releaseDocsIndex(RELEASE_DOCS), slugs: [] };
    expect(checkReleaseDocs(RELEASE_DOCS, release, DOC_TOPICS, null, stale).join("\n")).toContain("release-index");
  });

  it("refuses to generate, and writes nothing, when the tag is missing and the snapshot is for another release", () => {
    expect(() => refreshReleaseDocs(new URL("..", import.meta.url).pathname, { tag: "v9.9.9", version: "9.9.9" })).toThrow(/v9\.9\.9/);
    expect(() => refreshReleaseDocs(new URL("..", import.meta.url).pathname, null)).not.toThrow();
  });
});
