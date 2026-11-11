import baselineJson from "@/data/release-baseline.json";
import { FEATURE_CATALOGUE, compareVersions, type Feature } from "./features";
import { FOUNDER_SWITCH_VALUES, type FounderSwitchValues } from "./founder-switches";
import type { ModelFact, ProviderFact, RepoFacts } from "./facts";

export interface BaselineFeature {
  id: string;
  name: string;
  summary: string;
  status: string;
  since: string | null;
}

export interface ReleaseBaseline {
  format: string;
  tag: string;
  version: string;
  features: BaselineFeature[];
  providers: ProviderFact[];
  models: ModelFact[];
  toolCount: number | null;
}

export type CapabilityAvailability = "released" | "in_source" | "hidden";

export interface CapabilityContext {
  release: { tag: string; version: string } | null;
  baseline: ReleaseBaseline | null;
  switches?: FounderSwitchValues;
}

export interface ReleaseCounts {
  providerRoutes: number;
  models: number;
  tools: number | null;
}

export interface ReleaseView {
  providers: ProviderFact[];
  models: ModelFact[];
  counts: ReleaseCounts | null;
}

const BASELINE_FORMAT = "codewhale.release-baseline.v1";

const isString = (value: unknown): value is string => typeof value === "string" && value.length > 0;

export function parseReleaseBaseline(value: unknown): ReleaseBaseline | null {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  if (raw.format !== BASELINE_FORMAT || !isString(raw.tag) || !isString(raw.version) || raw.tag !== `v${raw.version}`
    || !Array.isArray(raw.features) || !Array.isArray(raw.providers) || !Array.isArray(raw.models)
    || !(raw.toolCount === null || (Number.isSafeInteger(raw.toolCount) && (raw.toolCount as number) > 0))) return null;
  const features = raw.features.every((row) => row && isString(row.id) && isString(row.name) && isString(row.summary) && isString(row.status)
    && (row.since === null || typeof row.since === "string"));
  const providers = raw.providers.every((row) => row && isString(row.id) && isString(row.label) && typeof row.env === "string");
  const models = raw.models.every((row) => row && isString(row.id));
  if (!features || !providers || !models || raw.providers.length === 0 || raw.models.length === 0) return null;
  return raw as unknown as ReleaseBaseline;
}

export const RELEASE_BASELINE: ReleaseBaseline | null = parseReleaseBaseline(baselineJson);

const VENDOR_AGENT_ACP = /(?:\b(?:Claude Code|Codex|Antigravity|Gemini CLI)\b[^.]*\bACP\b|\bACP\b[^.]*\b(?:Claude Code|Codex|Antigravity|Gemini CLI)\b)/i;

const CODEWHALE_PLANS = /\bcode ?whale (?:plans?|tiers?|pricing|subscriptions?|allowances?)\b/i;

const HELD_BY_SWITCH: Record<string, keyof FounderSwitchValues> = {
  "cloud-computer-dispatch": "cloudComputersBeyondInvite",
  "account-computer-files": "cloudComputersBeyondInvite",
};

export function isFounderHeld(feature: Pick<Feature, "id" | "name" | "summary">, switches: FounderSwitchValues = FOUNDER_SWITCH_VALUES): boolean {
  const key = HELD_BY_SWITCH[feature.id];
  if (key) return !switches[key];
  if (VENDOR_AGENT_ACP.test(`${feature.name}. ${feature.summary}`)) return !switches.namedVendorAgentsOverAcp;
  if (CODEWHALE_PLANS.test(`${feature.name}. ${feature.summary}`)) return !switches.plansPublished;
  return false;
}

export function baselineFor(context: CapabilityContext): ReleaseBaseline | null {
  const { baseline, release } = context;
  return baseline && release && baseline.tag === release.tag ? baseline : null;
}

export function capabilityAvailability(
  feature: Pick<Feature, "id" | "name" | "summary" | "since">,
  context: CapabilityContext,
): CapabilityAvailability {
  if (isFounderHeld(feature, context.switches)) return "hidden";
  const { release } = context;
  if (!release) return "hidden";
  const { since } = feature;
  if (since === "unreleased") return "in_source";
  if (since) {
    const comparison = compareVersions(since, release.version);
    if (comparison === null) return "hidden";
    return comparison <= 0 ? "released" : "in_source";
  }
  const baseline = baselineFor(context);
  if (!baseline) return "hidden";
  return baseline.features.some((row) => row.id === feature.id) ? "released" : "in_source";
}

export interface CapabilityView {
  feature: Feature;
  availability: Exclude<CapabilityAvailability, "hidden">;
  name: string;
  summary: string;
}

export function capabilityView(feature: Feature, context: CapabilityContext): CapabilityView | null {
  const availability = capabilityAvailability(feature, context);
  if (availability === "hidden") return null;
  const tagged = availability === "released" ? baselineFor(context)?.features.find((row) => row.id === feature.id) : undefined;
  return { feature, availability, name: tagged?.name ?? feature.name, summary: tagged?.summary ?? feature.summary };
}

export function capabilityViews(context: CapabilityContext, features: readonly Feature[] = FEATURE_CATALOGUE.features): CapabilityView[] {
  return features.flatMap((feature) => {
    const view = capabilityView(feature, context);
    return view ? [view] : [];
  });
}

export interface CapabilityClaim {
  feature: string;
  describesCurrentRow?: boolean;
}

export function claimAvailability(
  claim: CapabilityClaim,
  context: CapabilityContext,
  features: readonly Feature[] = FEATURE_CATALOGUE.features,
): CapabilityAvailability {
  const feature = features.find((row) => row.id === claim.feature);
  if (!feature) return "hidden";
  const availability = capabilityAvailability(feature, context);
  if (availability !== "released" || !claim.describesCurrentRow) return availability;
  const tagged = baselineFor(context)?.features.find((row) => row.id === feature.id);
  return tagged && tagged.summary === feature.summary ? "released" : "in_source";
}

export function runtimeCapabilityContext(facts: Pick<RepoFacts, "latestPublishedRelease">): CapabilityContext {
  const release = facts.latestPublishedRelease;
  return { release: release ? { tag: release.tag, version: release.version } : null, baseline: RELEASE_BASELINE };
}

export function releaseView(facts: Pick<RepoFacts, "latestPublishedRelease">, baseline: ReleaseBaseline | null = RELEASE_BASELINE): ReleaseView {
  const matched = baselineFor({ release: facts.latestPublishedRelease, baseline });
  if (!matched) return { providers: [], models: [], counts: null };
  return {
    providers: matched.providers,
    models: matched.models,
    counts: { providerRoutes: matched.providers.length, models: matched.models.length, tools: matched.toolCount },
  };
}

export function releasedFeatureNames(version: string): string[] {
  const context: CapabilityContext = { release: { tag: `v${version}`, version }, baseline: RELEASE_BASELINE };
  return capabilityViews(context)
    .filter((view) => view.availability === "released" && view.feature.status === "stable" && view.feature.since)
    .map((view) => view.name);
}

export function claimedItems<T extends { capability?: CapabilityClaim }>(
  items: readonly T[],
  context: CapabilityContext,
  features: readonly Feature[] = FEATURE_CATALOGUE.features,
): { item: T; availability: "released" | "in_source" }[] {
  return items.flatMap((item) => {
    const availability = item.capability ? claimAvailability(item.capability, context, features) : "released";
    return availability === "hidden" ? [] : [{ item, availability }];
  });
}
