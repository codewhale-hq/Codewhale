import { DurableObject } from "cloudflare:workers";
import handler from "./.open-next/worker.js";
import type { AgentEnv } from "./lib/community-agent-tasks";
import { runFactsDrift } from "./lib/facts-drift";
import { runLinkCheck, runSemanticDrift } from "./lib/content-watch";
import { fetchWithStaticInstaller } from "./lib/static-installer";
import { applyDraftLock, type DraftLockRequest } from "./lib/draft-claim-lock";

/**
 * Exclusive claim on one community-agent draft (binding DRAFT_CLAIM_LOCK, one
 * instance per draft via idFromName). OpenNext's custom-worker entry exports
 * extra Durable Object classes from here; see lib/draft-claim-lock.ts.
 */
export class DraftClaimLock extends DurableObject {
  async act(req: DraftLockRequest) {
    return applyDraftLock(this.ctx.storage, Date.now(), req);
  }
}

export default {
  fetch(request, env, ctx) {
    return fetchWithStaticInstaller(
      request,
      env,
      ctx,
      (nextRequest, nextEnv, nextCtx) => handler.fetch(nextRequest, nextEnv, nextCtx),
    );
  },
  async scheduled(event: ScheduledEvent, env: Record<string, unknown>, ctx: ExecutionContext) {
    const expr = event.cron;
    ctx.waitUntil((async () => {
      const agentEnv = env as unknown as AgentEnv;
      if (expr === "0 */6 * * *") {
        await runFactsDrift(agentEnv);
      }
      else if (expr === "0 0 * * *") {
        await runLinkCheck(agentEnv);
        await runSemanticDrift(agentEnv);
      }
    })());
  },
} satisfies ExportedHandler;
