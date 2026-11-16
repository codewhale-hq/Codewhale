import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  RELEASE_BASELINE,
  baselineFor,
  capabilityAvailability,
  capabilityViews,
  claimAvailability,
  claimedItems,
  isFounderHeld,
  parseReleaseBaseline,
  releaseView,
  releasedFeatureNames,
  runtimeCapabilityContext,
  type CapabilityContext,
  type ReleaseBaseline,
} from "./capabilities";
import { FACTS } from "./facts.generated";
import { FEATURE_CATALOGUE, type Feature } from "./features";
import { FOUNDER_SWITCHES, FOUNDER_SWITCH_VALUES } from "./founder-switches";
import { PRODUCT_COPY } from "./content/product";
import ModelsPage from "../app/[locale]/models/page";
import { buildLlmsTxt } from "./llms-txt";

vi.mock("next/navigation", () => ({ redirect: vi.fn(), notFound: vi.fn(), usePathname: () => "/en", useRouter: () => ({}) }));

const WEB = new URL("..", import.meta.url).pathname;

const row = (id: string, since: string | null, extra: Partial<Feature> = {}): Feature => ({
  id, name: `Name ${id}`, summary: `Summary ${id}`, status: "stable", surfaces: { tui: "stable" }, since, flag: null, docs: null, registryLine: 1, ...extra,
});

const baseline = (tag: string, ids: string[], summaries: Record<string, string> = {}): ReleaseBaseline => ({
  format: "codewhale.release-baseline.v1",
  tag,
  version: tag.slice(1),
  features: ids.map((id) => ({ id, name: `Tagged ${id}`, summary: summaries[id] ?? `Summary ${id}`, status: "stable", since: null })),
  providers: [{ id: "alpha", label: "Alpha", env: "ALPHA_KEY" }],
  models: [{ id: "alpha-1", provider: "Alpha", contextWindow: 1000, maxOutput: 100, reasoning: false, addedAt: "2026-01-01" }],
  toolCount: 7,
});

const at = (tag: string, ids: string[] = [], summaries: Record<string, string> = {}): CapabilityContext => ({
  release: { tag, version: tag.slice(1) },
  baseline: baseline("v1.2.0", ids, summaries),
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("a registry row is available only at or below the published release", () => {
  const fixture = row("fixture-capability", "1.3.0");

  it("never renders as available while its since version is above the published release", () => {
    expect(capabilityAvailability(fixture, at("v1.2.0"))).toBe("in_source");
    expect(capabilityViews(at("v1.2.0"), [fixture]).map((view) => view.availability)).toEqual(["in_source"]);
    expect(claimedItems([{ capability: { feature: fixture.id } }], at("v1.2.0"), [fixture])[0].availability).toBe("in_source");
  });

  it("renders as available at or below it", () => {
    expect(capabilityAvailability(row("older", "1.1.0"), at("v1.2.0"))).toBe("released");
    expect(capabilityAvailability(row("same", "1.2.0"), at("v1.2.0"))).toBe("released");
  });

  it("turns on when only the published release is raised", () => {
    const before = capabilityAvailability(fixture, at("v1.2.9"));
    const after = capabilityAvailability(fixture, at("v1.3.0"));
    expect([before, after]).toEqual(["in_source", "released"]);
  });

  it("treats unreleased as source only and an unknown release or malformed version as hidden", () => {
    expect(capabilityAvailability(row("pending", "unreleased"), at("v9.9.9"))).toBe("in_source");
    expect(capabilityAvailability(fixture, { release: null, baseline: null })).toBe("hidden");
    expect(capabilityAvailability(row("odd", "soon"), at("v9.9.9"))).toBe("hidden");
  });

  it("uses the registry at the published tag for rows with no recorded since", () => {
    const unrecorded = row("unrecorded", null);
    expect(capabilityAvailability(unrecorded, at("v1.2.0", ["unrecorded"]))).toBe("released");
    expect(capabilityAvailability(unrecorded, at("v1.2.0", ["other"]))).toBe("in_source");
    expect(capabilityAvailability(unrecorded, at("v1.3.0", ["unrecorded"]))).toBe("hidden");
    expect(capabilityAvailability(unrecorded, { release: { tag: "v1.2.0", version: "1.2.0" }, baseline: null })).toBe("hidden");
  });

  it("shows the wording the published release had, not the newer source wording", () => {
    const changed = row("changed", null, { summary: "Newer wording" });
    const context = at("v1.2.0", ["changed"], { changed: "Wording at the tag" });
    const [view] = capabilityViews(context, [changed]);
    expect(view.summary).toBe("Wording at the tag");
    expect(view.name).toBe("Tagged changed");
  });

  it("holds back a claim that describes the current row when the row changed since the release", () => {
    const changed = row("changed", null, { summary: "Newer wording" });
    const claim = { feature: "changed", describesCurrentRow: true };
    expect(claimAvailability(claim, at("v1.2.0", ["changed"], { changed: "Older wording" }), [changed])).toBe("in_source");
    expect(claimAvailability(claim, at("v1.2.0", ["changed"], { changed: "Newer wording" }), [changed])).toBe("released");
    expect(claimAvailability({ feature: "changed" }, at("v1.2.0", ["changed"], { changed: "Older wording" }), [changed])).toBe("released");
    expect(claimAvailability({ feature: "missing-row" }, at("v1.2.0"), [changed])).toBe("hidden");
  });
});

describe("founder holds stay hidden after a tag", () => {
  const far = at("v9.9.9");

  it("hides cloud computer rows and vendor agents over ACP, whatever the registry says", () => {
    const cloud = row("cloud-computer-dispatch", "0.1.0");
    const acp = row("acp-agents", "0.1.0", { summary: "Drive Claude Code and Codex as agents over ACP." });
    const acpReverse = row("acp-other", "0.1.0", { summary: "ACP lets Codewhale drive other agents such as Codex." });
    for (const feature of [cloud, acp, acpReverse]) {
      expect(isFounderHeld(feature), feature.id).toBe(true);
      expect(capabilityAvailability(feature, far), feature.id).toBe("hidden");
      expect(capabilityViews(far, [feature])).toEqual([]);
    }
    expect(capabilityAvailability(row("acp-client", "0.1.0", { summary: "ACP stdio connects compatible editors." }), far)).toBe("released");
  });

  it("hides rows about Codewhale plans or pricing until plans are published", () => {
    const plans = row("plan-row", "0.1.0", { summary: "Codewhale plans include a monthly allowance." });
    expect(capabilityAvailability(plans, far)).toBe("hidden");
    expect(capabilityAvailability(plans, { ...far, switches: { ...FOUNDER_SWITCH_VALUES, plansPublished: true } })).toBe("released");
    expect(capabilityAvailability(row("plan-mode", "0.1.0", { summary: "Plan and Work modes." }), far)).toBe("released");
  });

  it("opens only when the committed switch is turned on", () => {
    const acp = row("acp-agents", "0.1.0", { summary: "Drive Claude Code over ACP." });
    expect(capabilityAvailability(acp, { ...far, switches: { ...FOUNDER_SWITCH_VALUES, namedVendorAgentsOverAcp: true } })).toBe("released");
  });

  it("are committed off with a decision reference and not read from the environment", () => {
    for (const key of ["cloudComputersBeyondInvite", "namedVendorAgentsOverAcp", "plansPublished"] as const) {
      expect(FOUNDER_SWITCHES[key].value, key).toBe(false);
      expect(FOUNDER_SWITCHES[key].decision, key).toMatch(/§\d/);
    }
  });

  it("hold every real registry row about cloud computers", () => {
    const context = runtimeCapabilityContext(FACTS);
    const listed = new Set(capabilityViews(context).map((view) => view.feature.id));
    expect(listed.has("cloud-computer-dispatch")).toBe(false);
    expect(listed.has("account-computer-files")).toBe(false);
  });
});

describe("the committed registry and baseline", () => {
  const context = runtimeCapabilityContext(FACTS);

  it("has a baseline for exactly the published release", () => {
    expect(RELEASE_BASELINE).not.toBeNull();
    expect(baselineFor(context)?.tag).toBe(FACTS.latestPublishedRelease?.tag);
  });

  it("never lists a row newer than the published release as available", () => {
    const published = context.release!.version;
    const tagged = new Set(RELEASE_BASELINE!.features.map((feature) => feature.id));
    for (const view of capabilityViews(context, FEATURE_CATALOGUE.features)) {
      if (view.availability !== "released") continue;
      const { since, id } = view.feature;
      expect(since === "unreleased", id).toBe(false);
      if (since) {
        const [a, b, c] = since.split(".").map(Number);
        const [x, y, z] = published.split(".").map(Number);
        expect(a * 1e6 + b * 1e3 + c, id).toBeLessThanOrEqual(x * 1e6 + y * 1e3 + z);
      } else {
        expect(tagged.has(id), id).toBe(true);
      }
    }
  });

  it("keeps main-only rows out of the structured-data feature list", () => {
    const names = releasedFeatureNames(context.release!.version);
    for (const view of capabilityViews(context)) {
      if (view.availability === "in_source") expect(names).not.toContain(view.name);
    }
  });

  it("rejects a baseline that is malformed or names another format", () => {
    expect(parseReleaseBaseline(null)).toBeNull();
    expect(parseReleaseBaseline({ ...RELEASE_BASELINE, format: "other" })).toBeNull();
    expect(parseReleaseBaseline({ ...RELEASE_BASELINE, tag: "v0.0.1" })).toBeNull();
    expect(parseReleaseBaseline({ ...RELEASE_BASELINE, models: [] })).toBeNull();
  });

  it("every capability key in the product copy resolves to a registry row", () => {
    const ids = new Set(FEATURE_CATALOGUE.features.map((feature) => feature.id));
    const copy = [...PRODUCT_COPY.workPatterns, ...PRODUCT_COPY.modes, ...PRODUCT_COPY.permissions];
    for (const item of copy) expect(ids.has(item.capability!.feature), item.capability!.feature).toBe(true);
  });
});

describe("counts are the counts at the published release tag", () => {
  const view = releaseView(FACTS);

  it("come from the tagged source and name what they count", () => {
    expect(view.counts).toEqual({
      providerRoutes: RELEASE_BASELINE!.providers.length,
      models: RELEASE_BASELINE!.models.length,
      tools: RELEASE_BASELINE!.toolCount,
    });
    expect(view.providers).toHaveLength(view.counts!.providerRoutes);
  });

  it("are unknown, not guessed, when the baseline is for a different release", () => {
    const other = { latestPublishedRelease: { ...FACTS.latestPublishedRelease!, tag: "v9.9.9", version: "9.9.9" } };
    expect(releaseView(other)).toEqual({ providers: [], models: [], counts: null });
    expect(releaseView({ latestPublishedRelease: null })).toEqual({ providers: [], models: [], counts: null });
  });

  it("leave main-only models out of the models page", async () => {
    const tagged = new Set(RELEASE_BASELINE!.models.map((model) => model.id));
    const mainOnly = FACTS.models.filter((model) => !tagged.has(model.id));
    expect(mainOnly.length).toBeGreaterThan(0);
    const props = { params: Promise.resolve({ locale: "en" }), searchParams: Promise.resolve({}) } as never;
    const models = renderToStaticMarkup((await ModelsPage(props)) as never);
    for (const model of mainOnly) expect(models, model.id).not.toContain(`>${model.id}<`);
    expect(models).toContain(`Show all ${view.counts!.providerRoutes} provider routes`);
    expect(models).not.toMatch(/Show all \d+ providers/);
  });

  it("print provider routes in llms.txt at the tag", () => {
    const text = buildLlmsTxt();
    expect(text).toContain(`Built-in provider routes in ${FACTS.latestPublishedRelease!.tag} (${view.counts!.providerRoutes})`);
  });

  it("omit every counted sentence when the count is unknown", async () => {
    const other = { ...FACTS, latestPublishedRelease: { ...FACTS.latestPublishedRelease!, tag: "v9.9.9", version: "9.9.9" } };
    expect(releaseView(other).counts).toBeNull();
  });
});

describe("no literal count in copy", () => {
  const COUNT_WORDS = "(?:\\d[\\d,]*\\+?|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety|hundred)";
  const NOUNS = "(?:built-in )?(?:provider routes|providers|models|tools|native tools|locales|languages|routes)";
  const PATTERN = new RegExp(`\\b${COUNT_WORDS} ${NOUNS}\\b`, "i");
  const ZH = /\d+\s*(?:个|条|种)\s*(?:提供商|模型|工具|语言|路由)/;
  const ALLOWED: Record<string, string> = {
    "lib/content/tools.ts": "native tools for reading, editing",
    "lib/i18n/dictionaries/en/docs-mcp.ts": "listed those two tools",
  };

  function sources(dir: string): string[] {
    return readdirSync(join(WEB, dir)).flatMap((name) => {
      const path = join(dir, name);
      if (statSync(join(WEB, path)).isDirectory()) return sources(path);
      return /\.(ts|tsx)$/.test(name) && !/\.(test|generated)\./.test(name) ? [path] : [];
    });
  }

  it.each([...sources("lib/content"), ...sources("lib/i18n/dictionaries"), ...sources("app"), ...sources("components")])("%s", (path) => {
    const text = readFileSync(join(WEB, path), "utf8");
    const hit = text.split("\n").find((line) => !/^\s*(?:\*|\/\/|\/\*)/.test(line)
      && !(ALLOWED[path] && line.includes(ALLOWED[path])) && (PATTERN.test(line) || ZH.test(line)));
    expect(hit, hit).toBeUndefined();
  });
});
