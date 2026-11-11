import { readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  bodyDateLocale,
  bodyFallsBackToEnglish,
  docsChromeFallsBackToEnglish,
  docsEnglishTopics,
  DOCS_TOPICS,
  fallbackAttrs,
} from "./fallback";

describe("fallback helper", () => {
  it("treats English and Chinese bodies as translated, and a partial locale as English", () => {
    expect(bodyFallsBackToEnglish("/changelog", "en")).toBe(false);
    expect(bodyFallsBackToEnglish("/changelog", "zh")).toBe(false);
    expect(bodyFallsBackToEnglish("/changelog", "ar")).toBe(true);
    expect(bodyFallsBackToEnglish("/models", "ja")).toBe(true);
    expect(fallbackAttrs("/models", "ar")).toEqual({ lang: "en", dir: "ltr" });
    expect(fallbackAttrs("/models", "zh")).toBeUndefined();
    expect(fallbackAttrs("/models", "en")).toBeUndefined();
  });

  it("follows the page's own dictionary where one exists", () => {
    expect(bodyFallsBackToEnglish("/computer-use", "ar")).toBe(false);
    expect(bodyFallsBackToEnglish("/computer-use", "ja")).toBe(false);
    expect(bodyFallsBackToEnglish("/models", "ar")).toBe(true);
  });

  it("formats dates in English inside an English body only", () => {
    expect(bodyDateLocale("/models", "ar")).toBe(bodyDateLocale("/models", "en"));
    expect(bodyDateLocale("/models", "zh")).not.toBe(bodyDateLocale("/models", "en"));
  });

  it("labels the docs chrome English wherever the docs shell dictionary is missing", () => {
    expect(docsChromeFallsBackToEnglish("en")).toBe(false);
    expect(docsChromeFallsBackToEnglish("zh")).toBe(false);
    expect(docsChromeFallsBackToEnglish("ar")).toBe(true);
  });

  it("lists English docs topics for Arabic and none for English or Chinese", () => {
    const ar = docsEnglishTopics("ar");
    expect(ar).toContain("modes");
    expect(ar).toContain("");
    expect(ar).not.toContain("guide");
    expect(docsEnglishTopics("en")).toEqual([]);
    expect(docsEnglishTopics("zh")).not.toContain("modes");
    expect(docsEnglishTopics("zh")).toContain("runtime-reference");
  });

  it("names every docs topic directory", () => {
    const dir = fileURLToPath(new URL("../../app/[locale]/docs", import.meta.url));
    const topics = readdirSync(dir, { withFileTypes: true })
      .filter((entry) => entry.isDirectory() && !entry.name.startsWith("_"))
      .map((entry) => entry.name)
      .sort();
    expect([...DOCS_TOPICS].sort()).toEqual(topics);
  });
});
