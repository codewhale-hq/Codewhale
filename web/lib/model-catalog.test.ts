import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { parseModelCatalog } from "./model-catalog.mjs";

const source = JSON.parse(readFileSync(new URL("../../crates/config/assets/models_dev.bundled.json", import.meta.url), "utf8"));

describe("reviewed public model projection", () => {
  it("preserves all public identities, labels and proven source-support dates", () => {
    const rows = parseModelCatalog(source);
    expect(rows).toHaveLength(source._reviewed.public_models.length);
    expect(new Set(rows?.map(row => row.id)).size).toBe(rows?.length);
    for (const original of source._reviewed.public_models) {
      expect(rows?.find(row => row.id === original.id)).toMatchObject({ provider: original.label, addedAt: original.added_at });
    }
    expect(rows?.find(row => row.id === "kimi-k3")?.maxOutput).toBe(131072);
  });

  it("uses supplied exact source facts and labels without a build-time fallback", () => {
    const fixture = structuredClone(source);
    fixture._reviewed.public_models[0].label = "Exact pinned label";
    fixture._reviewed.intrinsic[fixture._reviewed.public_models[0].id.toLowerCase()].context_window = 123456;
    const row = parseModelCatalog(JSON.stringify(fixture))?.find(row => row.id === fixture._reviewed.public_models[0].id);
    expect(row?.provider).toBe("Exact pinned label");
    expect(row?.contextWindow).toBe(123456);
    expect(parseModelCatalog(null)).toBeNull();
    expect(parseModelCatalog("not json")).toBeNull();
    expect(parseModelCatalog({ models: source.models, providers: source.providers })).toBeNull();
  });

  it("does not union private configured or provider-scoped rows into public labels", () => {
    const fixture = structuredClone(source);
    fixture.providers.private = { models: { "private-label": { id: "private-label", reasoning: true, limit: { context: 999999 } } } };
    expect(parseModelCatalog(fixture)).toEqual(parseModelCatalog(source));
    fixture._reviewed.public_models.push({ id: "private-label", label: "Private", added_at: null, aliases: [] });
    expect(parseModelCatalog(fixture)).toBeNull();
  });

  it("refuses duplicate, hostile, zero, boolean and malformed-date facts", () => {
    const mutations = [
      (fixture: typeof source) => fixture._reviewed.public_models.push(fixture._reviewed.public_models[0]),
      (fixture: typeof source) => fixture._reviewed.public_models[0].id = "hostile\u202ename",
      (fixture: typeof source) => fixture._reviewed.public_models[0].label = "bad\u001b[31m",
      (fixture: typeof source) => fixture._reviewed.public_models[0].added_at = "2026-99-99",
      (fixture: typeof source) => fixture._reviewed.public_models[0].added_at = "2026-02-30",
      (fixture: typeof source) => fixture._reviewed.intrinsic[fixture._reviewed.public_models[0].id.toLowerCase()].max_output = true,
      (fixture: typeof source) => fixture._reviewed.intrinsic[fixture._reviewed.public_models[0].id.toLowerCase()].context_window = 0,
    ];
    for (const mutate of mutations) {
      const fixture = structuredClone(source);
      mutate(fixture);
      expect(parseModelCatalog(fixture)).toBeNull();
    }
  });
});
