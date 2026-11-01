/**
 * The exclusive hold on one community-agent draft while a maintainer action
 * (post or discard) runs.
 *
 * The `DraftClaimLock` Durable Object (exported from `worker.ts`, one instance
 * per draft identity via `idFromName`) runs `applyDraftLock` against its own
 * storage. A Durable Object handles one event at a time and its input gate
 * holds other events while a storage call is pending, so the read and write
 * below cannot interleave with another request's: exactly one claim wins.
 *
 * This module has no `cloudflare:workers` import so it runs under vitest and
 * the Next.js build; `worker.ts` holds the thin class around it.
 */

export type DraftLockAction = "post" | "discard" | "generate";

export interface PostAttempt { at: string; identity: string }

export type DraftLockRequest =
  | { op: "post-status" }
  | { op: "remember-post"; token: string; attempt: PostAttempt }
  | { op: "forget-post"; token: string }
  | { op: "claim"; token: string; action: DraftLockAction; leaseMs: number }
  /**
   * `holdMs` > 0 keeps refusing other claims for that long after an action
   * whose decision was recorded, covering the time the KV decision marker
   * takes to reach other locations. 0 frees the draft at once.
   */
  | { op: "release"; token: string; holdMs: number };

export type DraftLockResponse =
  | { ok: true; attempt?: PostAttempt }
  | { ok: false; holder: DraftLockAction };

/** The subset of `DurableObjectStorage` the lock uses. */
export interface DraftLockStorage {
  get<T>(key: string): Promise<T | undefined>;
  put<T>(key: string, value: T): Promise<void>;
  delete(key: string): Promise<boolean>;
}

interface Lease {
  token: string;
  action: DraftLockAction;
  /** Epoch ms. A lease past this no longer holds, so a crashed action cannot wedge the draft. */
  expiresAt: number;
}

const LEASE_KEY = "lease";

export async function applyDraftLock(
  storage: DraftLockStorage,
  now: number,
  req: DraftLockRequest
): Promise<DraftLockResponse> {
  if (req.op === "post-status") {
    return { ok: true, attempt: await storage.get<PostAttempt>("post-attempt") };
  }
  const lease = await storage.get<Lease>(LEASE_KEY);
  const live = lease && lease.expiresAt > now ? lease : undefined;

  if (req.op === "remember-post" || req.op === "forget-post") {
    if (!live || live.token !== req.token) throw new Error("post claim expired");
    if (req.op === "remember-post") {
      if (!/^[0-9a-f]{64}$/.test(req.attempt.identity) || !Number.isFinite(Date.parse(req.attempt.at))) throw new Error("invalid post receipt");
      const previous = await storage.get<PostAttempt>("post-attempt");
      if (previous && previous.identity !== req.attempt.identity) throw new Error("unresolved post has different text or target");
      await storage.put("post-attempt", previous ?? req.attempt);
    } else {
      await storage.delete("post-attempt");
    }
    return { ok: true };
  }
  if (req.op === "claim") {
    if (live && live.token !== req.token) return { ok: false, holder: live.action };
    await storage.put<Lease>(LEASE_KEY, {
      token: req.token,
      action: req.action,
      expiresAt: now + Math.max(0, req.leaseMs),
    });
    return { ok: true };
  }

  // Release: only the holder's own token can release, so a request whose
  // lease expired and was retaken cannot free the new holder's claim.
  if (!live || live.token !== req.token) return { ok: true };
  if (req.holdMs > 0) {
    await storage.put<Lease>(LEASE_KEY, { ...live, expiresAt: now + req.holdMs });
  } else {
    await storage.delete(LEASE_KEY);
  }
  return { ok: true };
}

/** The RPC surface of a `DraftClaimLock` stub. */
export interface DraftClaimLockStub {
  act(req: DraftLockRequest): Promise<DraftLockResponse>;
}

/** The `DRAFT_CLAIM_LOCK` Durable Object namespace binding, as the app uses it. */
export interface DraftClaimLockNamespace {
  idFromName(name: string): DraftClaimLockId;
  get(id: DraftClaimLockId): DraftClaimLockStub;
}

/** Opaque `DurableObjectId`. */
export interface DraftClaimLockId {
  toString(): string;
}

export async function withGenerationLease<T extends { ok?: unknown }>(
  namespace: DraftClaimLockNamespace,
  task: string,
  generate: () => Promise<T>,
): Promise<T | "busy" | "unavailable"> {
  let lock: DraftClaimLockStub;
  const token = `generate:${crypto.randomUUID()}`;
  try {
    lock = namespace.get(namespace.idFromName(`draft-generation:${task}`));
    const held = await lock.act({ op: "claim", token, action: "generate", leaseMs: 45 * 60 * 1000 });
    if (!held.ok) return "busy";
  } catch {
    return "unavailable";
  }
  let completed = false;
  try {
    const result = await generate();
    completed = result.ok === true;
    return result;
  } finally {
    // A release failure leaves the durable lease in place until its expiry.
    await lock.act({ op: "release", token, holdMs: completed ? 120_000 : 0 }).catch(() => undefined);
  }
}
