import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it } from "vitest";
import { GET as getTopicMarkdown, generateStaticParams } from "../app/docs-markdown/[topic]/route";
import { GET as getFullText } from "../app/llms-full.txt/route";
import { checkReleaseDocs, refreshReleaseDocs, releaseDocsIndex } from "../scripts/release-docs-lib.mjs";
import { AGENT_DOCS } from "./agent-docs";
import { RELEASE_DOCS, buildTopicMarkdown } from "./release-docs";
import { DOC_TOPICS } from "./docs-map";
import { FACTS } from "./facts.generated";
import { REPO_URL } from "./i18n/links";

const release = FACTS.latestPublishedRelease!;
const SNAPSHOT: typeof RELEASE_DOCS = JSON.parse(readFileSync(new URL("../data/agent-docs.release.json", import.meta.url), "utf8"));
const SERVED = SNAPSHOT.topics.filter((topic) => topic.slug !== "install");
const releasedText = SNAPSHOT.topics.flatMap((topic) => topic.sources.map((source) => source.content)).join("\n");
const mainOnly = [...new Set(AGENT_DOCS.filter((topic) => topic.slug !== "install").flatMap((topic) => topic.sources.flatMap((source) => source.content.split("\n"))))]
  .filter((line) => line.trim().length >= 40 && !releasedText.includes(line));

async function topicBody(slug: string): Promise<string> {
  const topic = `${slug}.md`;
  const response = await getTopicMarkdown(new Request(`https://example.test/docs-markdown/${topic}`), { params: Promise.resolve({ topic }) });
  expect(response.status, slug).toBe(200);
  return response.text();
}

describe("documents at the released tag", () => {
  it("is a snapshot of the released version and nothing else", () => {
    expect(SNAPSHOT.tag).toBe(release.tag);
    expect(SNAPSHOT.version).toBe(release.version);
    expect(RELEASE_DOCS).toEqual(SNAPSHOT);
  });

  it("serves the snapshot's documents in the full text and every topic", async () => {
    const full = await getFullText().text();
    expect(generateStaticParams()).toEqual(SNAPSHOT.topics.map((topic) => ({ topic: `${topic.slug}.md` })));
    expect(SERVED.length).toBeGreaterThan(0);
    for (const topic of SERVED) {
      const body = await topicBody(topic.slug);
      expect(topic.sources.length, topic.slug).toBeGreaterThan(0);
      for (const source of topic.sources) {
        expect(body.includes(source.content), `${topic.slug} ${source.path}`).toBe(true);
        expect(full.includes(source.content), `full ${source.path}`).toBe(true);
      }
    }
  });

  it("serves no line that exists only in the documents on main", async () => {
    const full = await getFullText().text();
    for (const line of mainOnly) expect(full.includes(line), line).toBe(false);
    for (const topic of SERVED) {
      const body = await topicBody(topic.slug);
      for (const line of mainOnly) expect(body.includes(line), `${topic.slug} ${line}`).toBe(false);
    }
  });

  it("links each source at the tag and never at main", () => {
    const body = buildTopicMarkdown("mcp")!;
    expect(body).toContain(`Source: ${REPO_URL}/blob/${release.tag}/docs/MCP.md`);
    expect(body).not.toContain("Source: " + REPO_URL + "/blob/main/");
  });

  describe("with a registered document the tag does not have", () => {
    const topic = RELEASE_DOCS.topics.find((entry) => entry.slug === "mcp")!;
    const saved = [...topic.missing];
    afterEach(() => {
      topic.missing.splice(0, topic.missing.length, ...saved);
    });

    it("says plainly that it is not part of the release", () => {
      const path = "docs/ADDED_AFTER_THE_TAG.md";
      expect(buildTopicMarkdown("mcp")).not.toContain(path);
      (topic.missing as string[]).push(path);
      expect(buildTopicMarkdown("mcp")).toContain(`${path} was added after ${release.tag} and is not part of this release`);
    });
  });

  it("answers 404 for a topic that is not in the snapshot", async () => {
    const response = await getTopicMarkdown(new Request("https://example.test/docs-markdown/nope.md"), { params: Promise.resolve({ topic: "nope.md" }) });
    expect(response.status).toBe(404);
  });

  it("fails its check on a tag mismatch and passes on the released tag", () => {
    const snapshot = JSON.parse(JSON.stringify(RELEASE_DOCS));
    expect(checkReleaseDocs(snapshot, release, DOC_TOPICS, snapshot)).toEqual([]);
    const next = { tag: "v99.0.0", version: "99.0.0" };
    const problems = checkReleaseDocs(snapshot, next, DOC_TOPICS, null);
    expect(problems.some((line) => line.includes(`is for ${release.tag}`) && line.includes("v99.0.0"))).toBe(true);
    const mismatched = checkReleaseDocs({ ...snapshot, tag: "v99.0.0" }, release, DOC_TOPICS, null, undefined, { allowShallow: true });
    expect(mismatched).toEqual([expect.stringContaining(`is for v99.0.0, the published release is ${release.tag}`)]);
    expect(checkReleaseDocs(null, release, DOC_TOPICS, null).length).toBeGreaterThan(0);
  });

  it("fails closed when the release tag is unavailable unless allowShallow is explicitly opted in", () => {
    const snapshot = JSON.parse(JSON.stringify(RELEASE_DOCS));
    expect(checkReleaseDocs(snapshot, release, DOC_TOPICS, null).join("\n")).toContain("could not be verified");
    expect(checkReleaseDocs(snapshot, release, DOC_TOPICS, null, undefined, { allowShallow: true })).toEqual([]);
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
    expect(checkReleaseDocs(RELEASE_DOCS, release, DOC_TOPICS, RELEASE_DOCS, releaseDocsIndex(RELEASE_DOCS))).toEqual([]);
    const stale = { ...releaseDocsIndex(RELEASE_DOCS), slugs: [] };
    expect(checkReleaseDocs(RELEASE_DOCS, release, DOC_TOPICS, RELEASE_DOCS, stale).join("\n")).toContain("release-index");
  });

  it("refuses to generate, and writes nothing, when the tag is missing and the snapshot is for another release", () => {
    expect(() => refreshReleaseDocs(new URL("..", import.meta.url).pathname, { tag: "v9.9.9", version: "9.9.9" })).toThrow(/v9\.9\.9/);
    expect(() => refreshReleaseDocs(new URL("..", import.meta.url).pathname, null)).not.toThrow();
  });
});
