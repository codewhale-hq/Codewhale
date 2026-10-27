/**
 * facts-drift.ts — runtime version of scripts/derive-facts.mjs.
 *
 * Fetches source-of-truth files from raw.githubusercontent.com on a schedule,
 * re-derives the same RepoFacts shape, compares to the value cached in KV (or
 * to the build-time fallback on first run), and if anything changed writes
 * the new facts to CURATED_KV under "facts:current". `getFacts()` accepts the
 * KV value only when its exact source provenance is at least as new as the
 * deployed build; published-release metadata is resolved separately.
 *
 * Mechanical drift (provider added, sandbox backend renamed, version bumped)
 * fixes itself within one cron tick — no redeploy. Semantic drift (a new
 * feature should be advertised on the homepage) is still left to humans.
 */
import { parseModelCatalog } from "./model-catalog.mjs";
import { parseProviderDescriptors } from "./provider-descriptors.mjs";
import { fetchBoundedText } from "./bounded-body";
import type {
  PublishedReleaseFact,
  RepoFacts,
} from "./facts.generated";
import { FACTS as BUILD_FACTS } from "./facts.generated";
import { isRepoFacts } from "./facts";

const RAW_ROOT = "https://raw.githubusercontent.com/codewhale-hq/CodeWhale";
const RELEASE_TAG_ROOT = "https://github.com/codewhale-hq/CodeWhale/releases/tag";
const KV_KEY = "facts:current";
const LOG_KEY = "facts:drift-log";

interface KVNamespace {
  get(k: string): Promise<string | null>;
  put(k: string, v: string, o?: { expirationTtl?: number }): Promise<void>;
}

interface SourceMarker {
  revision: string;
  committedAt: string;
}

async function fetchText(
  path: string,
  revision: string,
  ghToken?: string,
): Promise<string | null> {
  const headers: Record<string, string> = {
    "User-Agent": "codewhale-web-drift",
  };
  if (ghToken) headers["Authorization"] = `Bearer ${ghToken}`;
  try {
    const r = await fetchBoundedText(`${RAW_ROOT}/${revision}/${path}`, { headers });
    return r.ok ? r.text : null;
  } catch {
    return null;
  }
}

async function fetchSourceMarker(ghToken?: string): Promise<SourceMarker | null> {
  const headers: Record<string, string> = {
    Accept: "application/vnd.github+json",
    "User-Agent": "codewhale-web-drift",
    "X-GitHub-Api-Version": "2022-11-28",
  };
  if (ghToken) headers.Authorization = `Bearer ${ghToken}`;
  try {
    const response = await fetchBoundedText(
      "https://api.github.com/repos/codewhale-hq/CodeWhale/commits/main",
      { headers },
    );
    if (!response.ok) return null;
    const json = JSON.parse(response.text) as {
      sha?: string;
      commit?: { committer?: { date?: string } };
    };
    const revision = json.sha;
    const committedAt = json.commit?.committer?.date;
    if (
      !revision ||
      !/^[0-9a-f]{40}$/i.test(revision) ||
      !committedAt ||
      !Number.isFinite(Date.parse(committedAt))
    ) {
      return null;
    }
    return { revision, committedAt };
  } catch {
    return null;
  }
}

function deriveVersion(cargo: string): string | null {
  const m = cargo.match(/^version\s*=\s*"([^"]+)"/m);
  return m ? m[1] : null;
}

function deriveCrates(cargo: string): string[] {
  const block = cargo.match(/members\s*=\s*\[([\s\S]*?)\]/);
  if (!block) return [];
  return [...block[1].matchAll(/"crates\/([^"]+)"/g)].map((m) => m[1]).sort();
}

function deriveSandboxBackends(source: string): string[] {
  const marker = source.match(
    /pub const PUBLIC_SANDBOX_BACKENDS\s*:\s*&\[&str\]\s*=\s*&\[([\s\S]*?)\];/,
  );
  if (!marker) return [];
  return [...marker[1].matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

async function fetchLatestPublishedRelease(
  ghToken?: string,
): Promise<PublishedReleaseFact | null> {
  const headers: Record<string, string> = {
    Accept: "application/vnd.github+json",
    "User-Agent": "codewhale-web-drift",
    "X-GitHub-Api-Version": "2022-11-28",
  };
  if (ghToken) headers["Authorization"] = `Bearer ${ghToken}`;
  try {
    const r = await fetchBoundedText("https://api.github.com/repos/codewhale-hq/CodeWhale/releases/latest", { headers });
    if (!r.ok) return null;
    const j = JSON.parse(r.text) as {
      tag_name?: string;
      published_at?: string;
    };
    if (
      !j.tag_name ||
      !/^v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(j.tag_name) ||
      !j.published_at ||
      !Number.isFinite(Date.parse(j.published_at))
    ) {
      return null;
    }
    return {
      tag: j.tag_name,
      version: j.tag_name.slice(1),
      publishedAt: j.published_at,
      // Built from the tag, not `html_url`: GitHub answers with the repo's
      // canonical casing (`codewhale-hq/Codewhale`), which the exact-URL check in
      // isRepoFacts rejects, invalidating the whole KV snapshot.
      url: `${RELEASE_TAG_ROOT}/${j.tag_name}`,
    };
  } catch {
    return null;
  }
}

function deriveLicense(licText: string): string | null {
  const first = licText.split(/\r?\n/).find((l) => l.trim().length > 0);
  if (!first) return null;
  if (/^MIT License/i.test(first)) return "MIT";
  if (/Apache.*2\.0/i.test(first)) return "Apache-2.0";
  return first.trim();
}

function parseGeneratedFacts(source: string): Record<string, unknown> | null {
  const match = source.match(
    /export\s+const\s+FACTS(?:\s*:\s*RepoFacts)?\s*=\s*(\{[\s\S]*\})\s*;?\s*$/,
  );
  if (!match) return null;

  try {
    const parsed = JSON.parse(match[1]) as unknown;
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function deriveCountFromGeneratedFacts(facts: Record<string, unknown> | null, field: "toolCount" | "contributorCount"): number | null {
  const count = facts?.[field];
  return typeof count === "number" && Number.isSafeInteger(count) && count >= (field === "contributorCount" ? 1 : 0)
    ? count
    : null;
}

/**
 * Model presentation comes from the exact revision's reviewed catalog;
 * tool and contributor counts retain the existing generated-source boundary.
 */
export async function deriveFactsFromRemote(ghToken?: string): Promise<RepoFacts | null> {
  const source = await fetchSourceMarker(ghToken);
  if (!source) return null;

  const [cargo, providerMetadata, sandboxSource, npmPkg, licText, generatedFacts, modelCatalog, latestPublishedRelease] = await Promise.all([
    fetchText("Cargo.toml", source.revision, ghToken),
    fetchText("crates/config/assets/provider_descriptors.json", source.revision, ghToken),
    fetchText("crates/sandbox/src/lib.rs", source.revision, ghToken),
    fetchText("npm/codewhale/package.json", source.revision, ghToken),
    fetchText("LICENSE", source.revision, ghToken),
    fetchText("web/lib/facts.generated.ts", source.revision, ghToken),
    fetchText("crates/config/assets/models_dev.bundled.json", source.revision, ghToken),
    fetchLatestPublishedRelease(ghToken),
  ]);

  if (!cargo) return null;
  const providerData = parseProviderDescriptors(providerMetadata);
  if (!providerData) return null;
  const generated = generatedFacts ? parseGeneratedFacts(generatedFacts) : null;
  const toolCount = deriveCountFromGeneratedFacts(generated, "toolCount");
  const contributorCount = deriveCountFromGeneratedFacts(generated, "contributorCount");
  const models = parseModelCatalog(modelCatalog);
  // Never attach current-main provenance to build-time tool/model/contributor facts. The
  // checked-in generated snapshot is guarded by the exact revision's CI drift
  // check, so an absent or malformed value makes the whole derivation fail.
  if (toolCount === null || contributorCount === null || models === null) return null;

  const facts: RepoFacts = {
    generatedAt: new Date().toISOString(),
    sourceRevision: source.revision,
    sourceCommittedAt: source.committedAt,
    version: deriveVersion(cargo),
    crates: deriveCrates(cargo),
    sandboxBackends: sandboxSource
      ? deriveSandboxBackends(sandboxSource)
      : BUILD_FACTS.sandboxBackends,
    providers: providerData.providers,
    models,
    defaultModel: providerData.defaultModel,
    nodeEngines: (() => {
      try { return npmPkg ? JSON.parse(npmPkg).engines?.node ?? null : null; } catch { return null; }
    })(),
    toolCount,
    contributorCount,
    license: licText ? deriveLicense(licText) : BUILD_FACTS.license,
    latestPublishedRelease:
      latestPublishedRelease ?? BUILD_FACTS.latestPublishedRelease,
  };

  if (!facts.version || facts.crates.length === 0 || facts.providers.length === 0) {
    return null;
  }
  return facts;
}

interface DriftDiff {
  field: keyof RepoFacts;
  before: unknown;
  after: unknown;
}

function diff(a: RepoFacts, b: RepoFacts): DriftDiff[] {
  const fields: (keyof RepoFacts)[] = [
    "sourceRevision",
    "sourceCommittedAt",
    "version",
    "crates",
    "sandboxBackends",
    "providers",
    "models",
    "defaultModel",
    "nodeEngines",
    "toolCount",
    "contributorCount",
    "license",
    "latestPublishedRelease",
  ];
  const out: DriftDiff[] = [];
  for (const f of fields) {
    const av = JSON.stringify(a[f]);
    const bv = JSON.stringify(b[f]);
    if (av !== bv) out.push({ field: f, before: a[f], after: b[f] });
  }
  return out;
}

export interface FactsDriftResult {
  ok: boolean;
  changed?: boolean;
  diffs?: DriftDiff[];
  reason?: string;
}

export async function runFactsDrift(env: { CURATED_KV?: KVNamespace; GITHUB_TOKEN?: string }): Promise<FactsDriftResult> {
  if (!env.CURATED_KV) return { ok: false, reason: "CURATED_KV not bound" };

  const remote = await deriveFactsFromRemote(env.GITHUB_TOKEN);
  if (!remote) return { ok: false, reason: "remote derivation failed" };
  // getFacts() discards a snapshot isRepoFacts rejects, so never store one.
  // The scheduled handler drops this result, so say it here: otherwise the
  // cron stops refreshing KV with no signal at all.
  const { sourceRevision, sourceCommittedAt, version } = remote;
  if (!isRepoFacts(remote)) {
    const reason = "remote facts failed validation";
    console.warn(
      `[facts-drift] ${reason}; KV snapshot not refreshed ` +
        `(sourceRevision=${String(sourceRevision)}, ` +
        `sourceCommittedAt=${String(sourceCommittedAt)}, version=${String(version)})`,
    );
    return { ok: false, reason };
  }

  const cachedRaw = await env.CURATED_KV.get(KV_KEY);
  let cached: RepoFacts = BUILD_FACTS;
  if (cachedRaw) {
    try {
      const parsed = JSON.parse(cachedRaw) as unknown;
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        cached = parsed as RepoFacts;
      }
    } catch {
      // A truncated or legacy cache is replaced by the newly derived snapshot.
    }
  }

  // Overlapping runs can finish out of order; a snapshot from an older source
  // commit never replaces a newer one (KV has no compare-and-set, so this
  // narrows the race to the read-to-write window rather than closing it).
  const cachedAt = Date.parse(String(cached.sourceCommittedAt ?? ""));
  if (cachedRaw && Number.isFinite(cachedAt) && cachedAt > Date.parse(String(remote.sourceCommittedAt))) {
    return { ok: true, changed: false };
  }

  const diffs = diff(cached, remote);
  if (diffs.length === 0) {
    return { ok: true, changed: false };
  }

  // Write new facts. No TTL — they live until next drift overwrites them.
  await env.CURATED_KV.put(KV_KEY, JSON.stringify(remote));

  // Append to drift log (last 20 entries).
  try {
    const logRaw = await env.CURATED_KV.get(LOG_KEY);
    const log = logRaw ? (JSON.parse(logRaw) as Array<{ at: string; diffs: DriftDiff[] }>) : [];
    log.unshift({ at: remote.generatedAt, diffs });
    await env.CURATED_KV.put(LOG_KEY, JSON.stringify(log.slice(0, 20)));
  } catch {
    /* non-fatal */
  }

  return { ok: true, changed: true, diffs };
}
