import { revalidatePath } from "next/cache";
import { NextResponse } from "next/server";
import { BodyReadError, readBoundedBody } from "@/lib/bounded-body";
import {
  approveDigestRecord,
  claimDraft,
  clearDraftResolution,
  clearPostOutcomeUnknown,
  getPostOutcomeUnknown,
  markPostOutcomeUnknown,
  deleteDigestRecord,
  deleteDraft,
  getAgentEnv,
  getDraft,
  getDraftResolution,
  markDraftResolved,
  parseDraftKey,
  releaseDraftClaim,
  reviewedBodyHash,
  validateSession,
  type CommunityAgentEnv,
  type DraftClaimResult,
} from "@/lib/community-agent";

export const dynamic = "force-dynamic";

async function checkAuth(req: Request, env: CommunityAgentEnv): Promise<{ ok: boolean; status?: number; error?: string }> {
  if (!env.MAINTAINER_TOKEN) {
    return { ok: false, status: 503, error: "MAINTAINER_TOKEN not configured" };
  }

  const cookieHeader = req.headers.get("cookie") ?? "";
  let sid: string | undefined;
  for (const c of cookieHeader.split(";")) {
    const [name, ...rest] = c.trim().split("=");
    if (name === "mt_sid") {
      sid = rest.join("=");
      break;
    }
  }

  if (!sid || !(await validateSession(env.CURATED_KV, sid))) {
    return { ok: false, status: 401, error: "unauthorized" };
  }
  return { ok: true };
}

const ALLOWED_ORIGINS = new Set(["https://codewhale.net", "https://www.codewhale.net"]);
const MAX_BODY_BYTES = 65_536;

/** Refresh the ISR copy of /digest after its published set changes. */
function revalidateDigestPage() {
  try {
    revalidatePath("/[locale]/digest", "page");
  } catch (e) {
    // The page still refreshes on its hourly revalidate.
    console.error("digest revalidation failed", e);
  }
}

/**
 * A GitHub 4xx other than 408/429 means the post was not created, so the
 * claim can be released. A 5xx, 408 or 429 can come back after GitHub already
 * created the comment or issue, so its outcome is unknown.
 */
function githubDefinitelyRejected(status: number): boolean {
  return status >= 400 && status < 500 && status !== 408 && status !== 429;
}

/** The answer for a claim that was not taken. */
function claimRefused(result: Exclude<DraftClaimResult, { ok: true }>) {
  if (result.reason === "unconfirmed") {
    return NextResponse.json({ error: "could not confirm the draft claim; nothing was done, retry" }, { status: 503 });
  }
  if (result.reason === "resolved") {
    return NextResponse.json({ error: `draft already ${result.resolution.state}` }, { status: 409 });
  }
  return NextResponse.json(
    { error: "another request is acting on this draft; check GitHub, then retry in a minute" },
    { status: 409 }
  );
}

const discarded = () => NextResponse.json({ ok: true, action: "discarded" });

export async function POST(req: Request) {
  const env = await getAgentEnv();

  const origin = req.headers.get("origin");
  if (origin && !ALLOWED_ORIGINS.has(origin)) {
    return NextResponse.json({ error: "forbidden origin" }, { status: 403 });
  }

  const auth = await checkAuth(req, env);
  if (!auth.ok) {
    return NextResponse.json(
      { error: auth.error ?? "unauthorized" },
      { status: auth.status ?? 401, headers: { "Cache-Control": "no-store" } }
    );
  }

  if (!req.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
    return NextResponse.json({ error: "content-type must be application/json" }, { status: 415 });
  }

  // Count the real body bytes (Content-Length is only an early rejection)
  // and answer malformed JSON with a 400 instead of an unhandled 500.
  let body: unknown;
  try {
    const bytes = await readBoundedBody(req, MAX_BODY_BYTES);
    body = JSON.parse(new TextDecoder().decode(bytes));
  } catch (e) {
    if (e instanceof BodyReadError) {
      return NextResponse.json({ error: e.message }, { status: e.status });
    }
    return NextResponse.json({ error: "invalid JSON body" }, { status: 400 });
  }
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    return NextResponse.json({ error: "invalid JSON body" }, { status: 400 });
  }
  const { action, draftKey, editedBody, lang, reviewedSha256 } = body as {
    action?: unknown;
    draftKey?: unknown;
    editedBody?: unknown;
    lang?: unknown;
    reviewedSha256?: unknown;
  };

  if (action !== "post" && action !== "discard") {
    return NextResponse.json({ error: "unknown action" }, { status: 400 });
  }
  if (typeof draftKey !== "string" || !draftKey || draftKey.length > 256) {
    return NextResponse.json({ error: "missing or invalid draftKey" }, { status: 400 });
  }
  const parsedKey = parseDraftKey(draftKey);
  if (!parsedKey) {
    return NextResponse.json({ error: "invalid draftKey namespace" }, { status: 400 });
  }
  if (editedBody !== undefined && typeof editedBody !== "string") {
    return NextResponse.json({ error: "invalid editedBody" }, { status: 400 });
  }
  if (editedBody !== undefined && editedBody.length > MAX_BODY_BYTES) {
    return NextResponse.json({ error: "editedBody too long" }, { status: 413 });
  }
  if (lang !== undefined && lang !== "en" && lang !== "zh") {
    return NextResponse.json({ error: "invalid lang" }, { status: 400 });
  }
  if (typeof reviewedSha256 !== "string" || !/^[0-9a-f]{64}$/.test(reviewedSha256)) {
    return NextResponse.json({ error: "missing or invalid reviewedSha256" }, { status: 400 });
  }
  const reviewLang = lang === "zh" ? "zh" : "en";

  const resolution = await getDraftResolution(env.CURATED_KV, parsedKey.type, parsedKey.id);
  if (resolution) {
    if (action === "discard" && resolution.state === "discarded") return discarded();
    return claimRefused({ ok: false, reason: "resolved", resolution });
  }
  let result: DraftClaimResult;
  try {
    result = await claimDraft(env, parsedKey.type, parsedKey.id, action);
  } catch (e) {
    return NextResponse.json({ error: `could not claim draft: ${String(e)}` }, { status: 500 });
  }
  if (!result.ok) {
    if (action === "discard" && (
      (result.reason === "resolved" && result.resolution.state === "discarded") ||
      (result.reason === "held" && result.holder === "discard")
    )) return discarded();
    return claimRefused(result);
  }
  const heldClaim = result.claim;
  const loadReviewed = async () => {
    const draft = await getDraft(env.CURATED_KV, draftKey);
    if (!draft) return NextResponse.json({ error: "draft not found" }, { status: 404 });
    const originalBody = reviewLang === "zh" ? draft.bodyZh : draft.bodyEn;
    if ((await reviewedBodyHash(originalBody)) !== reviewedSha256 ||
        (heldClaim.revision && (await reviewedBodyHash(JSON.stringify(draft))) !== heldClaim.revision)) {
      return NextResponse.json({ error: "draft changed since it was loaded; reload and review it again" }, { status: 409 });
    }
    if (draft.posted) return NextResponse.json({ error: "draft already posted" }, { status: 409 });
    if (action === "post" && !env.MAINTAINER_GITHUB_PAT) {
      return NextResponse.json({ error: "MAINTAINER_GITHUB_PAT not configured" }, { status: 500 });
    }
    if (action === "post" && draft.type !== "digest" && !draft.targetNumber) {
      return NextResponse.json({ error: "no target number" }, { status: 400 });
    }
    return { draft, originalBody };
  };
  const reviewed = await loadReviewed().catch(async (error: unknown) => {
    await releaseDraftClaim(env.CURATED_KV, heldClaim).catch(() => undefined);
    throw error;
  });
  if (reviewed instanceof Response) {
    await releaseDraftClaim(env.CURATED_KV, heldClaim).catch(() => undefined);
    return reviewed;
  }
  const { draft, originalBody } = reviewed;

  if (action === "discard") {
    let recorded = false;
    try {
      // The marker stops the next cron run from regenerating this draft.
      await markDraftResolved(env.CURATED_KV, parsedKey.type, parsedKey.id, "discarded");
      await deleteDraft(env.CURATED_KV, draftKey);
      if (draft.type === "digest") {
        await deleteDigestRecord(env.CURATED_KV, draft.id);
      }
      recorded = true;
    } catch (e) {
      return NextResponse.json({ error: `discard failed: ${String(e)}` }, { status: 500 });
    } finally {
      // The marker (or, if it failed, the draft) now carries the decision.
      await releaseDraftClaim(env.CURATED_KV, heldClaim, { recorded }).catch(() => undefined);
    }
    if (draft.type === "digest") revalidateDigestPage();
    return discarded();
  }

  if (action === "post") {
    try {
      // Keeps the cron from regenerating the draft while the post is in flight.
      await markDraftResolved(env.CURATED_KV, parsedKey.type, parsedKey.id, "posting");
    } catch (e) {
      await releaseDraftClaim(env.CURATED_KV, heldClaim).catch(() => undefined);
      return NextResponse.json({ error: `could not claim draft: ${String(e)}` }, { status: 500 });
    }

    const commentBody = editedBody ?? originalBody;

    // GitHub has no idempotency key. If an earlier attempt's outcome was
    // unknown, the post it may have created is looked for (from shortly
    // before that attempt) instead of posting blind a second time.
    const identity = await reviewedBodyHash(JSON.stringify({
      repo: env.GITHUB_REPO ?? "codewhale-hq/CodeWhale", type: draft.type,
      target: draft.targetNumber, body: commentBody,
    }));
    let unknownAt: string | null;
    try {
      unknownAt = await getPostOutcomeUnknown(env, parsedKey.type, parsedKey.id, identity);
    } catch (error) {
      await clearDraftResolution(env.CURATED_KV, parsedKey.type, parsedKey.id).catch(() => undefined);
      await releaseDraftClaim(env.CURATED_KV, heldClaim).catch(() => undefined);
      const changed = error instanceof Error && error.message === "unresolved post has different text or target";
      return NextResponse.json({ error: changed
        ? "an earlier post with different text or target is unresolved; check GitHub before retrying; nothing was posted"
        : "could not read the earlier post receipt; nothing was posted" }, { status: changed ? 409 : 503 });
    }
    const lookSince = unknownAt ? new Date(Date.parse(unknownAt) - 10 * 60 * 1000).toISOString() : null;
    const githubFind = async (url: string, authScheme: "token" | "Bearer", matches: (item: Record<string, unknown>) => boolean): Promise<Record<string, unknown> | undefined> => {
      const signal = AbortSignal.timeout(30_000);
      for (let page = 1; page <= 10; page++) {
        const pageUrl = new URL(url);
        pageUrl.searchParams.set("page", String(page));
        const res = await fetch(pageUrl.toString(), {
          headers: {
            Accept: "application/vnd.github+json",
            Authorization: `${authScheme} ${env.MAINTAINER_GITHUB_PAT}`,
            "X-GitHub-Api-Version": "2022-11-28",
          }, signal,
        });
        if (!res.ok) throw new Error(`GitHub ${res.status}`);
        const list: unknown = JSON.parse(new TextDecoder().decode(await readBoundedBody(res, 2 * 1024 * 1024)));
        if (!Array.isArray(list) || list.some(item => !item || typeof item !== "object")) throw new Error("GitHub returned no valid list");
        const found = list.find(matches);
        if (found) return found;
        if (list.length < 100) return undefined;
      }
      // A capped search is unknown, never proof that GitHub lacks the post.
      throw new Error("GitHub reconciliation page limit exceeded");
    };
    // This request posted nothing, so the draft is free again.
    const lookupFailed = async () => {
      try {
        await clearDraftResolution(env.CURATED_KV, parsedKey.type, parsedKey.id);
        await releaseDraftClaim(env.CURATED_KV, heldClaim);
      } catch { /* the claim expires on its own */ }
      return NextResponse.json(
        { error: "could not check GitHub for the post an earlier attempt may have created; nothing was posted, retry" },
        { status: 502 }
      );
    };
    const alreadyPosted = "An earlier attempt whose outcome was unknown had already posted this; it was not posted again.";

    // After GitHub accepted the post, bookkeeping failures must not turn into
    // an error the maintainer would "fix" by posting again.
    const recordPosted = async (): Promise<string | undefined> => {
      try {
        await markDraftResolved(env.CURATED_KV, parsedKey.type, parsedKey.id, "posted");
        // The marker now carries the decision, so a later claim sees it; a
        // reopened draft (new activity clears the marker) is postable again.
        await env.CURATED_KV?.put(draftKey, JSON.stringify(draft), { expirationTtl: 60 * 60 * 24 * 7 });
        await clearPostOutcomeUnknown(env, parsedKey.type, parsedKey.id, heldClaim);
        await releaseDraftClaim(env.CURATED_KV, heldClaim, { recorded: true }).catch(() => undefined);
        return undefined;
      } catch (e) {
        return `Posted to GitHub, but saving the draft state failed (${String(e)}). Do not post it again; it may reappear as pending.`;
      }
    };

    const postToGitHub = async (url: string, payload: unknown, authScheme: "token" | "Bearer") => {
      await markPostOutcomeUnknown(env, parsedKey.type, parsedKey.id, heldClaim, identity);
      try {
        return await fetch(url, {
          method: "POST",
          headers: {
            Accept: "application/vnd.github+json",
            Authorization: `${authScheme} ${env.MAINTAINER_GITHUB_PAT}`,
            "X-GitHub-Api-Version": "2022-11-28",
            "Content-Type": "application/json",
          },
          signal: AbortSignal.timeout(30_000),
          body: JSON.stringify(payload),
        });
      } catch {
        // Outcome unknown: keep the short-lived claim so an immediate retry
        // cannot double-post.
        return null;
      }
    };
    const unknownOutcome = async (detail: string) => {
      // Remembered past the claim, so a later retry looks before posting.
      // The durable receipt was persisted before the outbound POST.
      return NextResponse.json(
        { error: `${detail}; check GitHub before retrying (retry unlocks in 15 minutes)` },
        { status: 502 }
      );
    };
    const githubFailed = async (res: Response) => {
      const text = await res.text().catch(() => "");
      if (!githubDefinitelyRejected(res.status)) {
        // Keep the claim: GitHub may have created the post anyway.
        return unknownOutcome(`GitHub ${res.status}: ${text}`);
      }
      try {
        await clearPostOutcomeUnknown(env, parsedKey.type, parsedKey.id, heldClaim);
        await clearDraftResolution(env.CURATED_KV, parsedKey.type, parsedKey.id);
        await releaseDraftClaim(env.CURATED_KV, heldClaim);
      } catch { /* receipt remains; the next retry must reconcile */ }
      return NextResponse.json({ error: `GitHub ${res.status}: ${text}` }, { status: 502 });
    };

    if (draft.type === "digest") {
      const digestBody = commentBody;
      const firstLine = digestBody.split("\n")[0].replace(/^#+\s*/, "").trim();
      const title = firstLine || `Weekly Digest ${draft.id}`;

      const digestRepo = env.GITHUB_REPO ?? "codewhale-hq/CodeWhale";
      const issuesUrl = `https://api.github.com/repos/${digestRepo}/issues`;

      let issue: { number?: number; html_url?: string } | undefined;
      if (lookSince) {
        try {
          const listUrl = `${issuesUrl}?labels=digest&state=all&since=${encodeURIComponent(lookSince)}&per_page=100`;
          const found = await githubFind(listUrl, "token", (item) => !item.pull_request && item.title === title && item.body === digestBody);
          if (found) {
            issue = {
              number: typeof found.number === "number" ? found.number : undefined,
              html_url: typeof found.html_url === "string" ? found.html_url : undefined,
            };
          }
        } catch {
          return lookupFailed();
        }
      }
      const reconciled = issue !== undefined;
      if (!issue) {
        let digestRes: Response | null;
        try { digestRes = await postToGitHub(issuesUrl, { title, body: digestBody, labels: ["digest"] }, "token"); }
        catch { return lookupFailed(); }
        if (!digestRes) return unknownOutcome("GitHub request failed");
        if (!digestRes.ok) return githubFailed(digestRes);
        issue = await digestRes.json().catch(() => ({})) as { number?: number; html_url?: string };
      }

      draft.posted = true;
      if (typeof issue.number === "number") draft.targetNumber = issue.number;
      if (typeof issue.html_url === "string") draft.targetUrl = issue.html_url;
      let warning = await recordPosted();
      if (reconciled) warning ??= alreadyPosted;

      // Publishing to /digest is the approval, for the language shown to the
      // maintainer only, and only of the record that renders to exactly the
      // text they reviewed. An edited digest is posted to GitHub but not
      // published there, since the record holds the unedited text.
      let published = false;
      if (editedBody === undefined || editedBody === originalBody) {
        try {
          const approval = await approveDigestRecord(env.CURATED_KV, draft, reviewLang);
          published = approval === "published";
          if (published) revalidateDigestPage();
          else if (approval === "missing") {
            warning ??= "Posted to GitHub, but the weekly record is gone, so /digest does not show it.";
          } else {
            warning ??= "Posted to GitHub, but the stored weekly record does not match the reviewed text, so /digest does not show it.";
          }
        } catch (e) {
          warning ??= `Posted to GitHub, but publishing the digest page failed (${String(e)}).`;
        }
      } else {
        warning ??= "Posted to GitHub. The text was edited, so /digest does not show this week.";
      }

      return NextResponse.json({
        ok: true,
        action: "posted",
        number: issue.number,
        url: issue.html_url,
        published,
        ...(warning ? { warning } : {}),
      });
    }

    const repo = env.GITHUB_REPO ?? "codewhale-hq/CodeWhale";
    const commentUrl = `https://api.github.com/repos/${repo}/issues/${draft.targetNumber}/comments`;

    let reconciled = false;
    if (lookSince) {
      try {
        const listUrl = `${commentUrl}?since=${encodeURIComponent(lookSince)}&per_page=100`;
        reconciled = (await githubFind(listUrl, "Bearer", (comment) => comment.body === commentBody)) !== undefined;
      } catch {
        return lookupFailed();
      }
    }
    if (!reconciled) {
      let ghRes: Response | null;
      try { ghRes = await postToGitHub(commentUrl, { body: commentBody }, "Bearer"); }
      catch { return lookupFailed(); }
      if (!ghRes) return unknownOutcome("GitHub request failed");
      if (!ghRes.ok) return githubFailed(ghRes);
    }

    // Mark as posted
    draft.posted = true;
    const warning = (await recordPosted()) ?? (reconciled ? alreadyPosted : undefined);

    return NextResponse.json({ ok: true, action: "posted", ...(warning ? { warning } : {}) });
  }

  // ALLOWED_ACTIONS guard above means this is unreachable.
  return NextResponse.json({ error: "unknown action" }, { status: 400 });
}
