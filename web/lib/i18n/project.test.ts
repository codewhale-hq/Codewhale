import { describe, expect, it } from "vitest";
import { locales } from "./config";
import { pickText } from "./dictionaries";
import { project as en } from "./dictionaries/en/project";
import { projectText } from "./project";
import { PROJECT_COPY, PROJECT_GOALS, CONTRIBUTION_ROUTES, STANDING_PROJECTS, PROJECT_RELEASES } from "../content/project";

describe("shared project translations", () => {
  it("resolves every project string for each routed locale without dropping tokens", () => {
    for (const key of Object.keys(en) as (keyof typeof en)[]) {
      const text = projectText(key);
      const tokens = en[key].match(/\{\w+\}/g)?.sort() ?? [];
      for (const locale of locales) {
        expect(text[locale], `${locale}.${key}`).toEqual(expect.any(String));
        expect(pickText(text, locale), `${locale}.${key}`).toBe(text[locale]);
        expect(text[locale]?.match(/\{\w+\}/g)?.sort() ?? [], `${locale}.${key}`).toEqual(tokens);
      }
    }
  });

  it("resolves page consumers through the same locale spine", () => {
    const content = [PROJECT_COPY.goalsScope, PROJECT_GOALS[0].body, CONTRIBUTION_ROUTES[0].have, STANDING_PROJECTS[0].body, PROJECT_RELEASES[0].body];
    for (const text of content) {
      for (const locale of locales.filter(locale => locale !== "en")) {
        expect(pickText(text, locale)).not.toBe(text.en);
      }
    }
    expect(pickText({ en: "English", zh: "中文" }, "ja")).toBe("English");
    expect(pickText(PROJECT_COPY.goalsScope, "unknown")).toBe(PROJECT_COPY.goalsScope.en);
  });
});
