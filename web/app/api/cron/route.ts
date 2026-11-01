import { NextResponse } from "next/server";
import { getEnv } from "@/lib/kv";
import {
  runCurate,
  runTriage,
  runPrReview,
  runStale,
  runDupes,
  runDigest,
  type AgentEnv,
} from "@/lib/community-agent-tasks";
import { runFactsDrift } from "@/lib/facts-drift";
import { runLinkCheck, runSemanticDrift } from "@/lib/content-watch";
import { safeEqual } from "@/lib/community-agent";

export const dynamic = "force-dynamic";

const TASKS = ["curate", "triage", "pr-review", "stale", "dupes", "digest", "facts-drift", "linkcheck", "semantic-drift"] as const;
type Task = (typeof TASKS)[number];

/**
 * Manual trigger surface for community-agent tasks.
 *
 * Usage:
 *   GET /api/cron?task=curate
 *   Header: x-cron-secret: <CRON_SECRET>
 *
 * Real cron scheduling is handled by worker.ts's scheduled() handler.
 */
export async function GET(req: Request) {
  const env = await getEnv();

  // Always require auth
  if (!env.CRON_SECRET) {
    return NextResponse.json(
      { error: "manual trigger disabled in production" },
      { status: 503 }
    );
  }

  const auth = req.headers.get("x-cron-secret") ?? "";
  if (!(await safeEqual(auth, env.CRON_SECRET))) {
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  }

  const { searchParams } = new URL(req.url);
  const task = searchParams.get("task");

  if (!task || !TASKS.includes(task as Task)) {
    return NextResponse.json(
      { error: `missing or invalid task. Allowed: ${TASKS.join(", ")}` },
      { status: 400 }
    );
  }

  // Build AgentEnv from the same shape expected by the task functions
  const agentEnv: AgentEnv = {
    CURATED_KV: env.CURATED_KV,
    DEEPSEEK_API_KEY: env.DEEPSEEK_API_KEY,
    DEEPSEEK_BASE_URL: env.DEEPSEEK_BASE_URL ?? process.env.DEEPSEEK_BASE_URL,
    DEEPSEEK_MODEL: env.DEEPSEEK_MODEL ?? process.env.DEEPSEEK_MODEL,
    GITHUB_TOKEN: env.GITHUB_TOKEN,
    CRON_SECRET: env.CRON_SECRET,
    GITHUB_REPO: env.GITHUB_REPO,
    MAINTAINER_TOKEN: undefined,
    MAINTAINER_GITHUB_PAT: undefined,
  };

  try {
    let result: Record<string, unknown>;
    switch (task) {
      case "curate":
        result = await runCurate(agentEnv);
        break;
      case "triage":
        result = await runTriage(agentEnv);
        break;
      case "pr-review":
        result = await runPrReview(agentEnv);
        break;
      case "stale":
        result = await runStale(agentEnv);
        break;
      case "dupes":
        result = await runDupes(agentEnv);
        break;
      case "digest":
        result = await runDigest(agentEnv);
        break;
      case "facts-drift":
        result = await runFactsDrift({ ...agentEnv, DRAFT_CLAIM_LOCK: env.DRAFT_CLAIM_LOCK }) as unknown as Record<string, unknown>;
        break;
      case "linkcheck":
        result = await runLinkCheck(agentEnv) as unknown as Record<string, unknown>;
        break;
      case "semantic-drift":
        result = await runSemanticDrift(agentEnv) as unknown as Record<string, unknown>;
        break;
      default:
        // unreachable — guarded by TASKS check above
        result = { error: "unknown task" };
    }
    return NextResponse.json({ ok: true, task, result });
  } catch (e) {
    return NextResponse.json({ ok: false, error: String(e) }, { status: 200 });
  }
}
