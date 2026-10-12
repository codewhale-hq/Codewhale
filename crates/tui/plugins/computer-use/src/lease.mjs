// Human/agent control lease — the input gate for a shared computer.
//
// On a Codewhale Computer, a person and the agent share one
// X display and one Chromium. The Engine owns the control lease and writes its
// current holder to a small JSON file (CODEWHALE_CU_LEASE_FILE, normally
// /run/cw/lease.json, owned by cw-engine). While a person holds it, every
// input tool refuses with `computer_busy_human_driving`; observation tools
// (screenshot, get_app_state, browser_screenshot, ...) keep working so the
// agent can watch and resume after hand-back.
//
// Unlike stop_computer_control, which is a one-way kill for the session, this
// refusal is reversible: when the holder goes back to the agent (or the human
// lease expires), input works again with no restart.
//
// File contract (written atomically by the Engine, read here):
//   {"holder":"human"|"agent"|null, "since":"<ISO>", "expires_at":"<ISO>"|null, "generation":<int>}
// Rules:
//   - no CODEWHALE_CU_LEASE_FILE: no lease concept (a local desktop), never refuses;
//   - file absent: nobody holds it, the agent may act;
//   - holder "human" and expires_at absent or in the future: refuse;
//   - file present but unreadable or malformed: fail closed (`computer_lease_unreadable`).
import fs from "node:fs";

export const HUMAN_DRIVING = "computer_busy_human_driving";
export const LEASE_UNREADABLE = "computer_lease_unreadable";

export function leaseFile(env = process.env) {
  const file = env.CODEWHALE_CU_LEASE_FILE;
  return typeof file === "string" && file.trim() ? file.trim() : null;
}

/** Read the lease. Returns {configured, state:"none"|"human"|"agent"|"unreadable", ...}. */
export function readLease({ file = leaseFile(), now = Date.now(), read = (f) => fs.readFileSync(f, "utf8") } = {}) {
  if (!file) return { configured: false, state: "none" };
  let raw;
  try { raw = read(file); } catch (error) {
    if (error?.code === "ENOENT") return { configured: true, state: "none", file };
    return { configured: true, state: "unreadable", file, reason: error?.code ?? String(error?.message ?? error) };
  }
  let lease;
  try { lease = JSON.parse(raw); } catch { return { configured: true, state: "unreadable", file, reason: "not JSON" }; }
  if (!lease || typeof lease !== "object" || Array.isArray(lease)) return { configured: true, state: "unreadable", file, reason: "not an object" };
  const holder = lease.holder ?? null;
  if (holder !== null && holder !== "human" && holder !== "agent") return { configured: true, state: "unreadable", file, reason: `unknown holder ${JSON.stringify(holder)}` };
  const expiresAt = lease.expires_at ?? null;
  let expiresMs = null;
  if (expiresAt !== null) {
    expiresMs = Date.parse(expiresAt);
    if (!Number.isFinite(expiresMs)) return { configured: true, state: "unreadable", file, reason: "bad expires_at" };
  }
  const base = { configured: true, file, since: lease.since ?? null, expires_at: expiresAt, generation: lease.generation ?? null };
  if (holder === "human" && (expiresMs === null || expiresMs > now)) return { ...base, state: "human" };
  if (holder === "human") return { ...base, state: "none", expired: true };
  return { ...base, state: holder === "agent" ? "agent" : "none" };
}

/**
 * The refusal for an input tool, or null when input may proceed. The shape is
 * {code, message, extra} so callers can raise it as their own error type.
 */
export function inputRefusal(tool, lease = readLease()) {
  if (lease.state === "human") {
    return {
      code: HUMAN_DRIVING,
      message: `a person is driving this computer — "${tool}" was not sent. Observation tools still work. Wait for hand-back, then observe again before acting; do not try to work around it.`,
      extra: { retryable: true, lease: { holder: "human", since: lease.since, expires_at: lease.expires_at, generation: lease.generation } },
    };
  }
  if (lease.state === "unreadable") {
    return {
      code: LEASE_UNREADABLE,
      message: `the control lease could not be read (${lease.reason}); input stays refused until it can be, because the computer may be in a person's hands`,
      extra: { retryable: true },
    };
  }
  return null;
}

/**
 * Watch the lease and call onHuman() when it passes to a person, so in-flight
 * input can be cancelled mid-gesture. Polls (the file is tiny and fs.watch is
 * unreliable across atomic renames). Returns a stop function.
 */
export function watchLease(onHuman, { file = leaseFile(), intervalMs = 200, read } = {}) {
  if (!file) return () => {};
  let last = readLease({ file, read }).state;
  const timer = setInterval(() => {
    const state = readLease({ file, read }).state;
    if (state !== last && (state === "human" || state === "unreadable")) {
      try { onHuman(state); } catch { /* the watcher must never take the server down */ }
    }
    last = state;
  }, intervalMs);
  timer.unref?.();
  return () => clearInterval(timer);
}
