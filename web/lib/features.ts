import generated from "@/data/features.generated.json";
import { REPO_URL } from "./i18n/links";

export type FeatureStatus = "stable" | "flagged" | "experimental";
export type SurfaceStatus = "stable" | "partial" | "preview" | "none";
export interface Feature {
  id: string;
  name: string;
  summary: string;
  status: FeatureStatus;
  surfaces: Partial<Record<"tui" | "desktop" | "web" | "api", SurfaceStatus>>;
  since: string | null;
  flag: string | null;
  docs: string | null;
  registryLine: number;
}

export const FEATURE_CATALOGUE = generated as {
  format: string;
  source: { path: string; sha256: string };
  features: Feature[];
};

export function featureDocsHref(feature: Feature): string {
  return `${REPO_URL}/blob/main/${feature.docs ?? `docs/features.toml#L${feature.registryLine}`}`;
}

export function compareVersions(left: string, right: string): number | null {
  const parts = (value: string) => /^\d+\.\d+\.\d+$/.test(value) ? value.split(".").map(Number) : null;
  const a = parts(left);
  const b = parts(right);
  if (!a || !b) return null;
  for (let i = 0; i < 3; i++) {
    if (a[i] !== b[i]) return a[i] - b[i];
  }
  return 0;
}

export function publishedFeatureNames(version: string): string[] {
  return FEATURE_CATALOGUE.features.filter((feature) => {
    if (feature.status !== "stable" || !feature.since) return false;
    const comparison = compareVersions(feature.since, version);
    return comparison !== null && comparison <= 0;
  }).map((feature) => feature.name);
}
