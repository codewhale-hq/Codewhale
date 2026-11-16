import { getAgentEnv, validateSession } from "@/lib/community-agent";
import { merchEnv } from "@/lib/merch/server";
import { operatorAction } from "@/lib/merch/operator";
import { MerchError, parseConfig } from "@/lib/merch/model";
import { readBoundedBody, BodyReadError } from "@/lib/bounded-body";

export const dynamic = "force-dynamic";
export async function POST(request: Request) {
  try {
    const authEnv = await getAgentEnv();
    const sid = request.headers.get("cookie")?.split(";").map(c => c.trim()).find(c => c.startsWith("mt_sid="))?.slice(7);
    if (!await validateSession(authEnv.CURATED_KV, sid, authEnv.MAINTAINER_TOKEN)) throw new MerchError(401, "unauthorized", "Maintainer sign-in is required.");
    const env = await merchEnv(), config = parseConfig(env.MERCH_CONFIG_JSON);
    if (!config || request.headers.get("origin") !== new URL(config.siteOrigin).origin || !request.headers.get("content-type")?.startsWith("application/json")) throw new MerchError(403, "request_blocked", "Use the approved store origin.");
    const raw = await readBoundedBody(request, 8192);
    let input: unknown;
    try { input = JSON.parse(new TextDecoder().decode(raw)); } catch { throw new MerchError(422, "invalid_request", "Invalid operation data."); }
    return Response.json(await operatorAction(env, input), { headers: { "Cache-Control": "no-store" } });
  } catch (error) {
    const known = error instanceof MerchError || error instanceof BodyReadError;
    return Response.json({ error: known ? error.message : "Operation unavailable; review the durable receipt before retrying.", code: error instanceof MerchError ? error.code : "operator_unavailable" }, { status: known ? error.status : 503, headers: { "Cache-Control": "no-store" } });
  }
}
