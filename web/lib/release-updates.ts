import { BUILD_FACTS, isPublishedRelease, newestPublishedRelease, type PublishedReleaseFact } from "./facts";
import { fetchLatestPublishedRelease } from "./published-release";

export type ReleaseUpdateState = {
  release: PublishedReleaseFact | null;
  status: "current" | "cached" | "unavailable";
};

export type ReleaseUpdatesSnapshot = {
  terminal: ReleaseUpdateState;
  computerUse: ReleaseUpdateState;
  fetchedAt: string | null;
};

function releaseState(release: PublishedReleaseFact | null, current = false): ReleaseUpdateState {
  return {
    release: release ? {
      tag: release.tag,
      version: release.version,
      publishedAt: release.publishedAt,
      url: release.url,
    } : null,
    status: release ? (current ? "current" : "cached") : "unavailable",
  };
}

export async function getReleaseUpdates(
  lastKnownTerminal?: PublishedReleaseFact | null,
): Promise<ReleaseUpdatesSnapshot> {
  const buildRelease = isPublishedRelease(BUILD_FACTS.latestPublishedRelease)
    ? BUILD_FACTS.latestPublishedRelease : null;
  if (process.env.NEXT_PHASE === "phase-production-build") {
    return {
      terminal: releaseState(buildRelease),
      computerUse: { release: null, status: "unavailable" },
      fetchedAt: null,
    };
  }
  const fallback = newestPublishedRelease(
    buildRelease,
    isPublishedRelease(lastKnownTerminal) ? lastKnownTerminal : null,
  );
  const [terminal, computerUse] = await Promise.all([
    fetchLatestPublishedRelease(),
    fetchLatestPublishedRelease(undefined, "computerUse"),
  ]);
  const latestTerminal = newestPublishedRelease(terminal, fallback);
  return {
    terminal: releaseState(latestTerminal, terminal !== null && latestTerminal?.tag === terminal.tag),
    computerUse: releaseState(computerUse, computerUse !== null),
    fetchedAt: new Date().toISOString(),
  };
}
