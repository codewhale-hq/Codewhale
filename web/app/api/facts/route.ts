import { NextResponse } from "next/server";
import { releaseView } from "@/lib/capabilities";
import { BUILD_FACTS, getFactsWithProvenance, type RepoFacts } from "@/lib/facts";
import { cloudFactsSummary } from "@/lib/cloud-facts";
import { getEnv } from "@/lib/kv";
import { liveRecord } from "@/lib/repo-activity";

export const dynamic = "force-dynamic";
export const revalidate = 0;

function summary(facts: RepoFacts) {
  return {
    generatedAt: facts.generatedAt,
    sourceRevision: facts.sourceRevision,
    sourceCommittedAt: facts.sourceCommittedAt,
    version: facts.version,
    providerCount: facts.providers.length,
    modelCount: facts.models.length,
    toolCount: facts.toolCount,
    contributorCount: facts.contributorCount,
  };
}

/** Public, credential-free source/deployment drift receipt. */
export async function GET() {
  const resolution = await getFactsWithProvenance();
  // Additive: the cloud facts channel currently served by /api/facts/v1/stable
  // (null when unconfigured or unverifiable), so deployed-facts checks can
  // later assert served == published.
  const cloudFacts = await cloudFactsSummary(await getEnv());
  return NextResponse.json(
    {
      schemaVersion: 1,
      deployed: summary(BUILD_FACTS),
      resolved: {
        ...summary(resolution.facts),
        source: resolution.source,
        reason: resolution.reason,
      },
      latestPublishedRelease: resolution.facts.latestPublishedRelease,
      releaseCounts: releaseView(resolution.facts).counts,
      cloudFacts,
      liveRecord: liveRecord(),
    },
    {
      headers: {
        "Cache-Control": "no-store",
      },
    },
  );
}
