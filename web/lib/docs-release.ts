import { claimAvailability, runtimeCapabilityContext, type CapabilityClaim, type CapabilityContext } from "./capabilities";
import { FEATURE_CATALOGUE, type Feature } from "./features";
import { getFacts } from "./facts";
import { REPO_URL } from "./i18n/links";
import type { DocsPageDict, DocsSection } from "./i18n/dictionaries/types";

export async function docsCapabilityContext(): Promise<CapabilityContext> {
  return runtimeCapabilityContext(await getFacts());
}

export async function publishedDocUrl(repoPath: string): Promise<string> {
  const tag = (await getFacts()).latestPublishedRelease?.tag ?? "main";
  return `${REPO_URL}/blob/${tag}/${repoPath}`;
}

export function showsReleasedText(until: CapabilityClaim | undefined, context: CapabilityContext, features: readonly Feature[] = FEATURE_CATALOGUE.features): boolean {
  return !until || claimAvailability(until, context, features) !== "released";
}

export type SourcePlacement = "body" | "source" | "hidden";

export function sourcePlacement(claim: CapabilityClaim, context: CapabilityContext, features: readonly Feature[] = FEATURE_CATALOGUE.features): SourcePlacement {
  const availability = claimAvailability(claim, context, features);
  if (availability === "released") return "body";
  return availability === "in_source" ? "source" : "hidden";
}

export interface ResolvedDocsPage {
  sections: readonly DocsSection[];
  sourceSections: readonly DocsSection[];
}

export function resolveDocsPage(
  t: Pick<DocsPageDict, "sections" | "sourceGroups">,
  context: CapabilityContext,
  features: readonly Feature[] = FEATURE_CATALOGUE.features,
): ResolvedDocsPage {
  const groups = (t.sourceGroups ?? []).map((group) => ({ group, placement: sourcePlacement(group.claim, context, features) }));
  return {
    sections: [
      ...t.sections.filter((section) => showsReleasedText(section.until, context, features)),
      ...groups.filter((entry) => entry.placement === "body").flatMap((entry) => entry.group.sections),
    ],
    sourceSections: groups.filter((entry) => entry.placement === "source").flatMap((entry) => entry.group.sections),
  };
}
