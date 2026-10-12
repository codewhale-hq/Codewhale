import { existsSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { DOC_TOPICS, docTopicHref } from "./docs-map";
import { DOC_TASKS, docTaskHaystack, taskTopic } from "./docs-tasks";
import { getDocsShell, EN_DOCS_SHELL } from "./i18n/dictionaries";

const webRoot = new URL("../", import.meta.url);

/** Locale-relative route → the app-router page that must exist for it. */
function pageFileFor(href: string): string {
  return `app/[locale]${href}/page.tsx`;
}

describe("task-based docs index", () => {
  it("points every task at a first-party page that exists", () => {
    expect(DOC_TASKS.length).toBeGreaterThanOrEqual(12);
    for (const task of DOC_TASKS) {
      expect(task.href.startsWith("/"), `${task.id} href`).toBe(true);
      expect(existsSync(new URL(pageFileFor(task.href), webRoot)), `${task.id}: ${task.href}`).toBe(true);
    }
  });

  it("keeps ids unique and every task attached to a registered topic", () => {
    const ids = DOC_TASKS.map((t) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const task of DOC_TASKS) {
      const topic = taskTopic(task);
      expect(topic, `${task.id} → ${task.topicId}`).toBeDefined();
      // A task that lives on its topic's own page must agree with docs-map
      // about that page's route, so the two registries cannot drift apart.
      if (topic && topic.hasPage) {
        expect(docTopicHref(topic, "en"), `${task.id} route`).toBe(`/en${task.href}`);
      }
    }
  });

  it("keeps labels, descriptions, and keywords bilingual", () => {
    for (const task of DOC_TASKS) {
      for (const pair of [task.label, task.description, task.keywords]) {
        expect(pair.en.trim().length, `${task.id} en`).toBeGreaterThan(0);
        expect(pair.zh.trim().length, `${task.id} zh`).toBeGreaterThan(0);
      }
    }
  });

  it("covers the PRD reference set: auth, computers, providers, trust, troubleshooting", () => {
    const topicIds = new Set(DOC_TASKS.map((t) => t.topicId));
    for (const id of ["auth", "computers", "providers", "trust", "troubleshooting", "subagents"]) {
      expect(topicIds.has(id), id).toBe(true);
      expect(DOC_TOPICS.some((t) => t.id === id && t.hasPage), `${id} page`).toBe(true);
    }
  });

  it("searches across both languages regardless of locale", () => {
    const hay = DOC_TASKS.map(docTaskHaystack);
    expect(hay.some((h) => h.includes("dispatch"))).toBe(true);
    expect(hay.some((h) => h.includes("云端"))).toBe(true);
    expect(hay.some((h) => h.includes("/docs/trust"))).toBe(true);
  });

  it("keeps the hub search strings dictionary-driven with token parity", () => {
    for (const locale of ["en", "zh", "ja", "und"]) {
      const t = getDocsShell(locale);
      expect(t.searchMatches).toContain("{matched}");
      expect(t.searchMatches).toContain("{total}");
      expect(t.searchMatches).toContain("{query}");
      expect(t.searchNoMatches).toContain("{query}");
      expect(t.helpSource).toContain("{name}");
      expect(t.releasePublished).toContain("{tag}");
      expect(t.releaseCandidate).toContain("{version}");
    }
    expect(getDocsShell("zh").tasksHeading).not.toBe(EN_DOCS_SHELL.tasksHeading);
  });
});
