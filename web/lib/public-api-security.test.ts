import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

function routeSource(path: string): string {
  return readFileSync(new URL(`../app/api/${path}/route.ts`, import.meta.url), "utf8");
}

describe("public API security contracts", () => {
  it("validates the draft namespace before admin discard can reach KV", () => {
    const source = routeSource("admin/post");
    expect(source).toContain("parseDraftKey(draftKey)");
    expect(source.indexOf("parseDraftKey(draftKey)")).toBeLessThan(
      source.indexOf("getDraft(env.CURATED_KV, draftKey)"),
    );
    expect(source.indexOf("getDraft(env.CURATED_KV, draftKey)")).toBeLessThan(
      source.indexOf("deleteDraft(env.CURATED_KV, draftKey)"),
    );
  });

  it("bounds the public login body before comparing the maintainer token", () => {
    const source = routeSource("admin/login");
    expect(source).toContain("readBoundedUrlEncodedForm(req, MAX_LOGIN_BODY_BYTES)");
    expect(source).not.toContain("req.formData()");
    expect(source.indexOf("readBoundedUrlEncodedForm(req, MAX_LOGIN_BODY_BYTES)")).toBeLessThan(
      source.indexOf("safeEqual(submitted, env.MAINTAINER_TOKEN)"),
    );
  });

  it("digest post: never returns ok:true for a no-op (happy path returns real url/number)", () => {
    const source = routeSource("admin/post");
    // The old false-success sentinel must be gone
    expect(source).not.toContain("digest-skipped");
    expect(source).not.toContain("Digest pages are not posted as comments");
    // Happy path must surface the real GitHub Issue outcome
    expect(source).toContain("number: issue.number");
    expect(source).toContain("url: issue.html_url");
    // The draft must be marked posted and stored back on success
    const digestBlock = source.slice(source.indexOf('draft.type === "digest"'));
    expect(digestBlock.indexOf("draft.posted = true")).toBeLessThan(
      digestBlock.indexOf('action: "posted"'),
    );
  });

  // "digest post: GitHub API failure surfaces a 502" is covered behaviorally
  // in community-agent-review-state.test.ts.
});
