import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { GET } from "../app/llms.txt/route";
import { DOC_TOPICS, docTopicHref } from "./docs-map";
import { DISCORD_URL, REPO_URL } from "./i18n/links";
import { IDENTITY_PHRASE, SITE_NAME, SITE_URL } from "./page-meta";
import { buildLlmsTxt } from "./llms-txt";
import robots from "../app/robots";

describe("llms.txt", () => {
  it("indexes first-party docs topics plus the remaining sitemap pages", () => {
    const body = buildLlmsTxt();

    expect(body.startsWith(`# ${SITE_NAME}`)).toBe(true);
    expect(body).toContain(`> ${IDENTITY_PHRASE}`);
    expect(body).toContain(REPO_URL);
    expect(body).toContain(DISCORD_URL);

    for (const topic of DOC_TOPICS) {
      if (topic.hasPage) {
        expect(body, topic.id).toContain(`${SITE_URL}${docTopicHref(topic, "en")}`);
        expect(body, topic.id).toContain(topic.description.en);
      } else {
        expect(body, topic.id).toContain(topic.label.en);
      }
    }

    for (const path of [
      "/en/app",
      "/en/faq",
      "/en/runtime",
      "/en/computer-use",
      "/en/constitution",
      "/en/roadmap",
      "/en/feed",
      "/en/digest",
      "/en/community",
      "/en/contribute",
      "/en/docs",
      "/en/legal/terms",
      "/en/legal/privacy",
    ]) {
      expect(body, path).toContain(`${SITE_URL}${path}`);
    }
    expect(body).not.toContain("/pricing");
  });

  it("serves the generated body as text/plain from the well-known route", async () => {
    const response = GET();
    expect(response.headers.get("content-type")).toBe("text/plain; charset=utf-8");
    expect(await response.text()).toBe(buildLlmsTxt());
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
