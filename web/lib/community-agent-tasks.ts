import { withGenerationLease, type DraftClaimLockNamespace } from "./draft-claim-lock";
import { OUTBOUND_TIMEOUT_MS } from "@/lib/bounded-body";
import { fetchRepoStats } from "@/lib/github";
import {
  agentChat,
  DIGEST_PROMPT,
  saveDraft,
  getDraftResolution,
  renderDigestBody,
  logUsage,
  type WeeklyDigestRecord,
  type AgentDraft,
  type DeepSeekEnv,
} from "@/lib/community-agent";

export interface AgentEnv {
  DRAFT_CLAIM_LOCK?: DraftClaimLockNamespace;
  CURATED_KV?: {
    get(k: string): Promise<string | null>;
    put(k: string, v: string, o?: { expirationTtl?: number }): Promise<void>;
    list(o?: { prefix?: string; limit?: number }): Promise<{ keys: { name: string }[] }>;
    delete(key: string): Promise<void>;
  };
  DEEPSEEK_API_KEY?: string;
  DEEPSEEK_BASE_URL?: string;
  DEEPSEEK_MODEL?: string;
  GITHUB_TOKEN?: string;
  CRON_SECRET?: string;
  GITHUB_REPO?: string;
  MAINTAINER_TOKEN?: string;
  MAINTAINER_GITHUB_PAT?: string;
}

function dsEnv(env: AgentEnv): DeepSeekEnv {
  return {
    baseUrl: env.DEEPSEEK_BASE_URL ?? process.env.DEEPSEEK_BASE_URL,
    model: env.DEEPSEEK_MODEL ?? process.env.DEEPSEEK_MODEL,
  };
}

async function generateDigest(env: AgentEnv): Promise<Record<string, unknown>> {
  const repo = env.GITHUB_REPO ?? "codewhale-hq/CodeWhale";
  const weekAgo = new Date(Date.now() - 7 * 24 * 60 * 60 * 1000).toISOString();

  // Compute week ID
  const now = new Date();
  const startOfYear = new Date(now.getFullYear(), 0, 1);
  const weekNum = Math.ceil(((now.getTime() - startOfYear.getTime()) / 86400000 + startOfYear.getDay() + 1) / 7);
  const weekId = `${now.getFullYear()}-W${String(weekNum).padStart(2, "0")}`;

  try {
    if (await getDraftResolution(env.CURATED_KV, "digest", weekId)) {
      return { ok: true, skipped: true, weekId, reason: "digest already reviewed" };
    }

    const [issuesRes, pullsRes, stats] = await Promise.all([
      fetch(
        `https://api.github.com/repos/${repo}/issues?state=all&since=${weekAgo}&per_page=50&sort=updated&direction=desc`,
        {
          signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS),
          headers: {
            Accept: "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "codewhale-web",
            ...(env.GITHUB_TOKEN ? { Authorization: `Bearer ${env.GITHUB_TOKEN}` } : {}),
          },
        }
      ),
      fetch(
        `https://api.github.com/repos/${repo}/pulls?state=all&sort=updated&direction=desc&per_page=50`,
        {
          signal: AbortSignal.timeout(OUTBOUND_TIMEOUT_MS),
          headers: {
            Accept: "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "codewhale-web",
            ...(env.GITHUB_TOKEN ? { Authorization: `Bearer ${env.GITHUB_TOKEN}` } : {}),
          },
        }
      ),
      fetchRepoStats(env.GITHUB_TOKEN),
    ]);

    const issues = (await issuesRes.json()) as { number: number; title: string; state: string; pull_request?: unknown; created_at: string; user: { login: string } }[];
    const pulls = (await pullsRes.json()) as { number: number; title: string; state: string; merged_at?: string; created_at: string; user: { login: string } }[];

    const weekIssues = issues.filter((i) => !i.pull_request && new Date(i.created_at) > new Date(weekAgo));
    const weekPRs = pulls.filter((p) => new Date(p.created_at) > new Date(weekAgo));
    const mergedPRs = pulls.filter((p) => p.merged_at && new Date(p.merged_at) > new Date(weekAgo));

    const contributors = new Set([
      ...weekIssues.map((i) => i.user.login),
      ...weekPRs.map((p) => p.user.login),
    ]);

    const payload = {
      period: `${weekAgo.slice(0, 10)} — ${new Date().toISOString().slice(0, 10)}`,
      stats: { stars: stats.stars, forks: stats.forks },
      newIssues: weekIssues.map((i) => ({ number: i.number, title: i.title, author: i.user.login })),
      newPRs: weekPRs.map((p) => ({ number: p.number, title: p.title, author: p.user.login })),
      mergedPRs: mergedPRs.map((p) => ({ number: p.number, title: p.title })),
      contributors: [...contributors],
    };

    const { content, usage } = await agentChat(
      [{ role: "system", content: DIGEST_PROMPT }, { role: "user", content: JSON.stringify(payload) }],
      env.DEEPSEEK_API_KEY!,
      true,
      dsEnv(env)
    );

    const parsed = JSON.parse(content) as { titleEn: string; titleZh: string; summaryEn: string; summaryZh: string; sections: { heading: string; items: string[] }[] };

    const draft: AgentDraft = {
      id: weekId,
      type: "digest",
      bodyEn: renderDigestBody(parsed, "en"),
      bodyZh: renderDigestBody(parsed, "zh"),
      generatedAt: new Date().toISOString(),
      posted: false,
    };

    // Stage the structured digest for the weekly page, unapproved. The page
    // renders it only after the maintainer posts the draft from /admin.
    // Pick fields explicitly: model output must never be able to set
    // `approved` or any other record key.
    const record: WeeklyDigestRecord = {
      titleEn: parsed.titleEn,
      titleZh: parsed.titleZh,
      summaryEn: parsed.summaryEn,
      summaryZh: parsed.summaryZh,
      sections: parsed.sections,
      weekId,
      generatedAt: draft.generatedAt,
      approved: false,
    };
    if (!(await saveDraft(env, draft, { digest: record }))) {
      return { ok: true, skipped: true, weekId, reason: "digest already under review or resolved" };
    }

    await logUsage(env.CURATED_KV, usage.input, usage.output);
    return { ok: true, weekId };
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}

/** The existing durable claim authority also serializes each bounded cron batch.
 * Missing bindings stop generation before provider spend; KV is never a lock.
 * The 45-minute lease covers at most ten 180-second model calls and reads.
 * A completed batch retains a short hold for its KV drafts to propagate.
 */
async function withGenerationClaim(env: AgentEnv, task: string, generate: () => Promise<Record<string, unknown>>): Promise<Record<string, unknown>> {
  if (!env.DEEPSEEK_API_KEY) return { skipped: true, reason: "DEEPSEEK_API_KEY not set" };
  if (!env.DRAFT_CLAIM_LOCK || !env.CURATED_KV) return { skipped: true, reason: "durable generation claim unavailable; no provider call started" };
  const result = await withGenerationLease(env.DRAFT_CLAIM_LOCK, task, generate);
  if (result === "busy") return { ok: true, processed: 0, skipped: 1, reason: "generation already in progress or recently completed" };
  if (result === "unavailable") return { skipped: true, reason: "durable generation claim unavailable; no provider call started" };
  return result;
}

export const runDigest = (env: AgentEnv) => withGenerationClaim(env, "digest", () => generateDigest(env));
