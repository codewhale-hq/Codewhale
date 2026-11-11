import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { getHome } from "../i18n/dictionaries";
import type { CapabilityContext } from "../capabilities";
import type { DesktopReleaseFact } from "../desktop-release";
import type { Feature } from "../features";
import { PRODUCT_MAP_IDS, PRODUCT_MAP_STATE_KEY, PRODUCT_MAP_TEXT_KEYS, productMap } from "./product-map";

const feature = (id: string, since: string | null): Feature => ({
  id, name: id, summary: id, status: "stable", surfaces: { tui: "stable" }, since, flag: null, docs: null, registryLine: 1,
});

const FEATURES = [feature("subagents", "0.1.0"), feature("fleet", "0.9.0"), feature("workflows", "0.9.0")];

const released: CapabilityContext = { release: { tag: "v0.10.2", version: "0.10.2" }, baseline: null };

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

const states = (rows: ReturnType<typeof productMap>) => Object.fromEntries(rows.map((row) => [row.id, row.state]));

describe("the labelled product map", () => {
  it("lists the six parts in order with the locked states today", () => {
    const rows = productMap({ capabilities: released, features: FEATURES });
    expect(rows.map((row) => row.id)).toEqual([...PRODUCT_MAP_IDS]);
    expect(states(rows)).toEqual({
      terminal: "released",
      localBrowser: "released",
      manyAgents: "released",
      desktop: "developing",
      cloud: "inviteOnly",
      oneSession: "beingBuilt",
    });
    expect(rows.every((row) => row.sentenceHolds)).toBe(true);
  });

  it("turns only the desktop row to released when a desktop release object is valid", () => {
    const before = productMap({ capabilities: released, features: FEATURES });
    const after = productMap({ capabilities: released, features: FEATURES, surface: { desktopRelease } });
    expect(states(after)).toEqual({ ...states(before), desktop: "released" });
    expect(after.find((row) => row.id === "desktop")!.sentenceHolds).toBe(false);
  });

  it("keeps the desktop row in development for an invalid release object", () => {
    const invalid = { ...desktopRelease, assets: [] };
    expect(states(productMap({ capabilities: released, features: FEATURES, surface: { desktopRelease: invalid } })).desktop).toBe("developing");
  });

  it("reads many agents as released only when sub-agents, Fleets and workflows are all in the published release", () => {
    const older: CapabilityContext = { release: { tag: "v0.8.0", version: "0.8.0" }, baseline: null };
    expect(states(productMap({ capabilities: older, features: FEATURES })).manyAgents).toBe("developing");
    expect(states(productMap({ capabilities: { release: null, baseline: null }, features: FEATURES })).manyAgents).toBe("beingBuilt");
    expect(states(productMap({ capabilities: released, features: FEATURES.slice(0, 2) })).manyAgents).toBe("beingBuilt");
  });

  it("never reads one session as released, whatever else is released", () => {
    const rows = productMap({ capabilities: released, features: FEATURES, surface: { desktopRelease } });
    expect(states(rows).oneSession).toBe("beingBuilt");
  });

  it("has no state typed into a row", () => {
    const source = readFileSync(new URL("./product-map.ts", import.meta.url), "utf8");
    expect(source).not.toMatch(/state:\s*"/);
  });

  it("maps every part and state onto a dictionary string in every locale", () => {
    for (const locale of ["en", "zh", "ar"]) {
      const home = getHome(locale);
      for (const id of PRODUCT_MAP_IDS) {
        expect(home[PRODUCT_MAP_TEXT_KEYS[id].name].length, `${locale} ${id}`).toBeGreaterThan(0);
        expect(home[PRODUCT_MAP_TEXT_KEYS[id].sentence].length, `${locale} ${id}`).toBeGreaterThan(0);
      }
      for (const key of Object.values(PRODUCT_MAP_STATE_KEY)) expect(home[key].length).toBeGreaterThan(0);
    }
  });
});
