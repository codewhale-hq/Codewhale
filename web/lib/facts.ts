import {
  FACTS as BUILD_TIME_FACTS,
  type PublishedReleaseFact,
  type ReleaseAssetFact,
  type RepoFacts,
  type ProviderFact,
  type ModelFact,
} from "./facts.generated";

const KV_KEY = "facts:current";

export type { PublishedReleaseFact, ReleaseAssetFact, RepoFacts, ProviderFact, ModelFact };

export interface BuildProvenance {
  revision?: string | null;
  committedAt?: string | null;
  generatedAt?: string | null;
}

export function withBuildProvenance(base: RepoFacts, stamp: BuildProvenance): RepoFacts {
  const { revision, committedAt, generatedAt } = stamp;
  if (
    typeof revision !== "string" || !/^[0-9a-f]{40}$/i.test(revision) ||
    typeof committedAt !== "string" || !Number.isFinite(Date.parse(committedAt)) ||
    typeof generatedAt !== "string" || !Number.isFinite(Date.parse(generatedAt))
  ) {
    return base;
  }
  return { ...base, sourceRevision: revision, sourceCommittedAt: committedAt, generatedAt };
}

export const BUILD_FACTS: RepoFacts = withBuildProvenance(BUILD_TIME_FACTS, {
  revision: process.env.NEXT_PUBLIC_CODEWHALE_SOURCE_REVISION,
  committedAt: process.env.NEXT_PUBLIC_CODEWHALE_SOURCE_COMMITTED_AT,
  generatedAt: process.env.NEXT_PUBLIC_CODEWHALE_FACTS_GENERATED_AT,
});

interface KVNamespace {
  get(key: string): Promise<string | null>;
  put(key: string, value: string, opts?: { expirationTtl?: number }): Promise<void>;
}

export type FactsSource = "build" | "kv";

export interface FactsResolution {
  facts: RepoFacts;
  source: FactsSource;
  reason:
    | "no-kv-snapshot"
    | "invalid-kv-snapshot"
    | "kv-missing-source-provenance"
    | "build-missing-source-provenance"
    | "same-source-revision"
    | "kv-source-newer"
    | "build-source-newer-or-ambiguous";
  buildSourceRevision: string | null;
  kvSourceRevision: string | null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isModelFact(value: unknown): value is ModelFact {
  if (!isRecord(value)) return false;
  return (
    typeof value.id === "string" &&
    (value.provider === null || typeof value.provider === "string") &&
    (value.contextWindow === null || typeof value.contextWindow === "number") &&
    (value.maxOutput === null || typeof value.maxOutput === "number") &&
    typeof value.reasoning === "boolean" &&
    (value.addedAt === null || typeof value.addedAt === "string")
  );
}

export function isPublishedRelease(value: unknown): value is PublishedReleaseFact {
  if (!isRecord(value)) return false;
  return (
    typeof value.tag === "string" &&
    /^v\d+\.\d+\.\d+$/.test(value.tag) &&
    typeof value.version === "string" &&
    value.tag === `v${value.version}` &&
    typeof value.publishedAt === "string" &&
    Number.isFinite(Date.parse(value.publishedAt)) &&
    value.url === `https://github.com/codewhale-hq/CodeWhale/releases/tag/${value.tag}` &&
    (value.readAt === undefined ||
      (typeof value.readAt === "string" && Number.isFinite(Date.parse(value.readAt)))) &&
    (value.assets === undefined ||
      (Array.isArray(value.assets) &&
        value.assets.every(
          (asset) =>
            isRecord(asset) &&
            typeof asset.name === "string" &&
            typeof asset.size === "number" &&
            Number.isSafeInteger(asset.size) &&
            asset.size >= 0 &&
            (asset.sha256 === null ||
              (typeof asset.sha256 === "string" && /^[0-9a-f]{64}$/.test(asset.sha256))),
        )))
  );
}

export function isRepoFacts(value: unknown): value is RepoFacts {
  if (!isRecord(value)) return false;
  const sourceRevisionValid =
    value.sourceRevision === null ||
    (typeof value.sourceRevision === "string" && /^[0-9a-f]{40}$/i.test(value.sourceRevision));
  const sourceCommittedAtValid =
    value.sourceCommittedAt === null ||
    (typeof value.sourceCommittedAt === "string" &&
      Number.isFinite(Date.parse(value.sourceCommittedAt)));
  const sourceProvenancePaired =
    (value.sourceRevision === null) === (value.sourceCommittedAt === null);

  return (
    typeof value.generatedAt === "string" &&
    sourceRevisionValid &&
    sourceCommittedAtValid &&
    sourceProvenancePaired &&
    typeof value.version === "string" &&
    Array.isArray(value.crates) &&
    Array.isArray(value.sandboxBackends) &&
    Array.isArray(value.providers) &&
    value.providers.every(
      (provider) =>
        isRecord(provider) &&
        typeof provider.id === "string" &&
        typeof provider.label === "string" &&
        typeof provider.env === "string",
    ) &&
    Array.isArray(value.models) &&
    value.models.every(isModelFact) &&
    (value.defaultModel === null || typeof value.defaultModel === "string") &&
    (value.nodeEngines === null || typeof value.nodeEngines === "string") &&
    (value.toolCount === null || typeof value.toolCount === "number") &&
    typeof value.contributorCount === "number" &&
    Number.isSafeInteger(value.contributorCount) && value.contributorCount > 0 &&
    (value.license === null || typeof value.license === "string") &&
    (value.latestPublishedRelease === null ||
      isPublishedRelease(value.latestPublishedRelease))
  );
}

export function newestPublishedRelease(
  build: PublishedReleaseFact | null,
  cached: PublishedReleaseFact | null,
): PublishedReleaseFact | null {
  if (!build) return cached;
  if (!cached) return build;
  return Date.parse(cached.publishedAt) > Date.parse(build.publishedAt) ? cached : build;
}

/**
 * Select the mechanical fact snapshot independently from the latest published
 * release. A legacy or malformed KV value cannot replace a newer build merely
 * because it exists. Source commit timestamps provide ordering, while an exact
 * revision match permits the runtime snapshot for the same source.
 */
export function resolveFacts(build: RepoFacts, cached: unknown): FactsResolution {
  if (cached == null) {
    return {
      facts: build,
      source: "build",
      reason: "no-kv-snapshot",
      buildSourceRevision: build.sourceRevision,
      kvSourceRevision: null,
    };
  }
  if (!isRepoFacts(cached)) {
    return {
      facts: build,
      source: "build",
      reason: "invalid-kv-snapshot",
      buildSourceRevision: build.sourceRevision,
      kvSourceRevision: null,
    };
  }

  const latestPublishedRelease = newestPublishedRelease(
    build.latestPublishedRelease,
    cached.latestPublishedRelease,
  );
  const withLatestRelease = (facts: RepoFacts): RepoFacts => ({
    ...facts,
    latestPublishedRelease,
  });

  if (!cached.sourceRevision || !cached.sourceCommittedAt) {
    return {
      facts: withLatestRelease(build),
      source: "build",
      reason: "kv-missing-source-provenance",
      buildSourceRevision: build.sourceRevision,
      kvSourceRevision: cached.sourceRevision,
    };
  }
  if (!build.sourceRevision || !build.sourceCommittedAt) {
    return {
      facts: withLatestRelease(build),
      source: "build",
      reason: "build-missing-source-provenance",
      buildSourceRevision: build.sourceRevision,
      kvSourceRevision: cached.sourceRevision,
    };
  }
  if (cached.sourceRevision === build.sourceRevision) {
    return {
      facts: withLatestRelease(cached),
      source: "kv",
      reason: "same-source-revision",
      buildSourceRevision: build.sourceRevision,
      kvSourceRevision: cached.sourceRevision,
    };
  }

  const buildTime = Date.parse(build.sourceCommittedAt);
  const cachedTime = Date.parse(cached.sourceCommittedAt);
  if (Number.isFinite(buildTime) && Number.isFinite(cachedTime) && cachedTime > buildTime) {
    return {
      facts: withLatestRelease(cached),
      source: "kv",
      reason: "kv-source-newer",
      buildSourceRevision: build.sourceRevision,
      kvSourceRevision: cached.sourceRevision,
    };
  }

  return {
    facts: withLatestRelease(build),
    source: "build",
    reason: "build-source-newer-or-ambiguous",
    buildSourceRevision: build.sourceRevision,
    kvSourceRevision: cached.sourceRevision,
  };
}

async function getKv(): Promise<KVNamespace | undefined> {
  if (process.env.NEXT_PHASE === "phase-production-build") {
    return undefined;
  }

  try {
    const mod = await import("@opennextjs/cloudflare");
    const ctx = await mod.getCloudflareContext({ async: true });
    return (ctx.env as { CURATED_KV?: KVNamespace }).CURATED_KV;
  } catch {
    return undefined;
  }
}

export async function getFactsWithProvenance(): Promise<FactsResolution> {
  try {
    const kv = await getKv();
    if (!kv) return resolveFacts(BUILD_FACTS, null);
    const raw = await kv.get(KV_KEY);
    if (!raw) return resolveFacts(BUILD_FACTS, null);
    return resolveFacts(BUILD_FACTS, JSON.parse(raw) as unknown);
  } catch {
    return resolveFacts(BUILD_FACTS, null);
  }
}

/** Resolved facts for the current request, with fail-safe build precedence. */
export async function getFacts(): Promise<RepoFacts> {
  return (await getFactsWithProvenance()).facts;
}
