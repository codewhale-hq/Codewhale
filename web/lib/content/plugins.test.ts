import { describe, expect, it } from "vitest";
import { PLUGINS_COMING, PLUGINS_COMING_VERSION, pluginsComingApplies } from "./plugins";

describe("plugins page, what 0.10.3 adds", () => {
  it("shows the coming section only while the published release is older than 0.10.3", () => {
    expect(PLUGINS_COMING_VERSION).toBe("0.10.3");
    expect(pluginsComingApplies(null)).toBe(true);
    expect(pluginsComingApplies({ version: "0.10.2" })).toBe(true);
    expect(pluginsComingApplies({ version: "0.10.3" })).toBe(false);
    expect(pluginsComingApplies({ version: "0.11.0" })).toBe(false);
  });

  it("carries English and Chinese for every item", () => {
    for (const item of PLUGINS_COMING) {
      for (const text of [item.title, item.detail]) {
        expect(text.en.trim().length).toBeGreaterThan(0);
        expect(text.zh.trim().length).toBeGreaterThan(0);
        expect(text.zh).not.toBe(text.en);
      }
    }
  });
});
