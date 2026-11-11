import { readBoundedBody } from "../bounded-body";

/**
 * product-usage.ts — aggregate, default-on, user-disableable usage counting
 * for the website.
 *
 * This is the browser side of the first-party telemetry contract
 * (telemetry-ingest/src/schema.ts, docs/TELEMETRY.md): closed schema version
 * 5 carrying policy notice version 7, the unchanged `product_usage` event with thirteen
 * unsigned counters, a random v4 install id unrelated to any person and
 * rotated every 90 days, and fixed aggregate `funnel_usage` counters — no URL, referrer, error
 * text, account, or content ever enters the envelope. There is no analytics
 * SDK and no processor token in the browser; the same-origin route
 * (app/api/product-telemetry) forwards a validated batch to the canonical
 * ingest only when the operator has configured that exact endpoint.
 *
 * Counting is on by default and every recorded opt-out stays off. The only
 * stored state is the person's own choice: an explicit "off" from any policy
 * version disables counting, an explicit "on" keeps it, and the absence of a
 * record is the default. Unreadable stored state fails closed. The notice
 * version is policy metadata, never a record that anyone accepted anything.
 * Turning counting off clears the queued counts and the install id, cancels
 * any pending delivery, and — through the `storage` event — does the same in
 * every other open tab.
 *
 * Framework-free and injectable so the contract is testable in Node: the
 * storage, clock, id source, and transport are parameters with browser
 * defaults.
 */

export const SCHEMA_VERSION = 5;
export const LEGACY_SCHEMA_VERSION = 3;
export const NOTICE_VERSION = 7;
export const LEGACY_NOTICE_VERSION = 5;
export const INSTALL_ID_ROTATION_MS = 90 * 24 * 60 * 60 * 1000;
export const MAX_ENVELOPE_BYTES = 4 * 1024;
/** Counts wait this long after the last interaction before one delivery. */
export const FLUSH_DELAY_MS = 20_000;

/** Kept under its historical key so an opt-out recorded under the old policy still counts. */
export const PREFERENCE_STORAGE_KEY = "cw-usage-consent";
export const INSTALL_STORAGE_KEY = "cw-usage-install";
export const COUNTERS_STORAGE_KEY = "cw-usage-counters";
export const FUNNEL_STORAGE_KEY = "cw-usage-funnel-counters";

export const PRODUCT_COUNTER_FIELDS = [
  "page_view",
  "docs_view",
  "install_copy",
  "download",
  "signup",
  "login",
  "session_create",
  "session_resume",
  "turn_submit",
  "turn_complete",
  "settings_open",
  "integration_connect",
  "error_shown",
] as const;

export const FUNNEL_COUNTER_FIELDS = [
  "home_view", "install_view", "quickstart_view", "contribute_view",
  "install_intent", "install_confirmed", "star_exit", "issue_exit",
  "help_wanted_exit", "guide_exit", "docs_search", "docs_search_empty",
  "docs_search_result", "quickstart_confirmed",
] as const;

export type FunnelCounter = (typeof FUNNEL_COUNTER_FIELDS)[number];
export type FunnelCounters = Record<FunnelCounter, number>;

export type ProductCounter = (typeof PRODUCT_COUNTER_FIELDS)[number];
export type ProductCounters = Record<ProductCounter, number>;

export const SURFACES = ["website", "web-app", "desktop"] as const;
export type Surface = (typeof SURFACES)[number];

export const ENVELOPE_FIELDS = [
  "schema_version",
  "notice_version",
  "sent_at",
  "install_id",
  "app_version",
  "git_sha",
  "surface",
  "os",
  "arch",
  "libc",
  "tty",
  "events",
] as const;

export interface ProductUsageEnvelope {
  schema_version: 3 | 5;
  notice_version: 5 | 7;
  sent_at: string;
  install_id: string;
  app_version: string;
  git_sha: null;
  surface: Surface;
  os: "other";
  arch: "other";
  libc: "none";
  tty: false;
  events: ({ event: "product_usage"; counters: ProductCounters } | { event: "funnel_usage"; counters: FunnelCounters })[];
}

const U32_MAX = 4294967295;
const SENT_AT_RE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/;
const INSTALL_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const VERSION_RE = /^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/;

export function emptyCounters(): ProductCounters {
  return Object.fromEntries(PRODUCT_COUNTER_FIELDS.map((field) => [field, 0])) as ProductCounters;
}

export function emptyFunnelCounters(): FunnelCounters {
  return Object.fromEntries(FUNNEL_COUNTER_FIELDS.map((field) => [field, 0])) as FunnelCounters;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function keysExactly(value: Record<string, unknown>, expected: readonly string[]): string | null {
  const actual = Object.keys(value);
  for (const key of actual) if (!expected.includes(key)) return `unexpected key ${key}`;
  for (const key of expected) if (!(key in value)) return `missing key ${key}`;
  return null;
}

/**
 * The closed-set validator, mirroring the ingest's rules for a browser
 * batch: exact key sets everywhere, constant envelope values, bounded aggregate
 * `product_usage` and `funnel_usage` events, every counter a u32. Unknown keys reject the whole
 * envelope — there is no sanitising path.
 */
export function validateEnvelope(
  value: unknown,
  options: { surfaces?: readonly Surface[] } = {},
): { ok: true; envelope: ProductUsageEnvelope } | { ok: false; reason: string } {
  const surfaces = options.surfaces ?? SURFACES;
  if (!isPlainObject(value)) return { ok: false, reason: "not an object" };
  const keyError = keysExactly(value, ENVELOPE_FIELDS);
  if (keyError) return { ok: false, reason: `envelope: ${keyError}` };
  const legacy = value.schema_version === LEGACY_SCHEMA_VERSION;
  if (!legacy && value.schema_version !== SCHEMA_VERSION) return { ok: false, reason: "schema_version" };
  if (value.notice_version !== (legacy ? LEGACY_NOTICE_VERSION : NOTICE_VERSION)) return { ok: false, reason: "notice_version" };
  if (typeof value.sent_at !== "string" || !SENT_AT_RE.test(value.sent_at)) return { ok: false, reason: "sent_at" };
  if (typeof value.install_id !== "string" || !INSTALL_ID_RE.test(value.install_id)) return { ok: false, reason: "install_id" };
  if (typeof value.app_version !== "string" || value.app_version.length > 64 || !VERSION_RE.test(value.app_version)) {
    return { ok: false, reason: "app_version" };
  }
  if (value.git_sha !== null) return { ok: false, reason: "git_sha" };
  if (typeof value.surface !== "string" || !surfaces.includes(value.surface as Surface) || (!legacy && value.surface !== "website")) return { ok: false, reason: "surface" };
  if (value.os !== "other") return { ok: false, reason: "os" };
  if (value.arch !== "other") return { ok: false, reason: "arch" };
  if (value.libc !== "none") return { ok: false, reason: "libc" };
  if (value.tty !== false) return { ok: false, reason: "tty" };
  if (!Array.isArray(value.events) || value.events.length < 1 || value.events.length > (legacy ? 1 : 200)) return { ok: false, reason: "events" };
  for (const event of value.events) {
    if (!isPlainObject(event)) return { ok: false, reason: "event: not an object" };
    const eventKeyError = keysExactly(event, ["event", "counters"]);
    if (eventKeyError) return { ok: false, reason: `event: ${eventKeyError}` };
    const fields = event.event === "product_usage" ? PRODUCT_COUNTER_FIELDS : !legacy && event.event === "funnel_usage" ? FUNNEL_COUNTER_FIELDS : null;
    if (!fields) return { ok: false, reason: "event: name" };
    if (!isPlainObject(event.counters)) return { ok: false, reason: "counters: not an object" };
    const counterKeyError = keysExactly(event.counters, fields);
    if (counterKeyError) return { ok: false, reason: `counters: ${counterKeyError}` };
    for (const field of fields) {
      const item = event.counters[field];
      if (typeof item !== "number" || !Number.isInteger(item) || item < 0 || item > U32_MAX) {
        return { ok: false, reason: `counters: ${field}` };
      }
    }
  }
  return { ok: true, envelope: value as unknown as ProductUsageEnvelope };
}

/** RFC3339 UTC at second precision, exactly `to_rfc3339_opts(Secs, true)`. */
export function sentAt(now: number): string {
  return new Date(Math.floor(now / 1000) * 1000).toISOString().replace(/\.\d{3}Z$/, "Z");
}

export function buildEnvelope(input: {
  counters: ProductCounters;
  funnel?: FunnelCounters;
  installId: string;
  appVersion: string;
  surface: Surface;
  now: number;
}): ProductUsageEnvelope {
  return {
    schema_version: SCHEMA_VERSION,
    notice_version: NOTICE_VERSION,
    sent_at: sentAt(input.now),
    install_id: input.installId,
    app_version: input.appVersion,
    git_sha: null,
    surface: input.surface,
    os: "other",
    arch: "other",
    libc: "none",
    tty: false,
    events: [
      { event: "product_usage", counters: { ...input.counters } },
      ...(input.funnel && hasFunnelCounts(input.funnel) ? [{ event: "funnel_usage" as const, counters: { ...input.funnel } }] : []),
    ],
  };
}

// --------------------------------------------------------------- preference

export interface UsagePreferenceRecord {
  /** Policy notice version in force when the choice was made. Metadata only. */
  version: number;
  granted: boolean;
  decidedAt: string;
}

/** `default` is the absence of a record: counting is on. `on` / `off` are explicit choices. */
export type UsagePreference = "on" | "off" | "default";

/**
 * Reads the stored choice. Counting is on by default, so no record means
 * `default`. Any explicit refusal — from this policy version or an older one
 * — stays `off`; an old decline is still a decline. A record that exists but
 * cannot be read fails closed as `off` rather than being replaced by the
 * default.
 */
export function readUsagePreference(raw: string | null | undefined): UsagePreference {
  if (raw === null || raw === undefined) return "default";
  try {
    const parsed = JSON.parse(raw) as Partial<UsagePreferenceRecord>;
    if (!isPlainObject(parsed) || typeof parsed.granted !== "boolean") return "off";
    return parsed.granted ? "on" : "off";
  } catch {
    return "off";
  }
}

export function usageCountingEnabled(preference: UsagePreference): boolean {
  return preference !== "off";
}

export function usagePreferenceRecord(granted: boolean, now: number): string {
  const record: UsagePreferenceRecord = { version: NOTICE_VERSION, granted, decidedAt: sentAt(now) };
  return JSON.stringify(record);
}

export interface PrivacySignalSource {
  globalPrivacyControl?: unknown;
  doNotTrack?: unknown;
  msDoNotTrack?: unknown;
}

const DO_NOT_TRACK_VALUES = ["1", "yes"];

export function privacySignalActive(
  navigatorLike: object | null | undefined = typeof navigator === "undefined" ? null : navigator,
  windowLike: object | null | undefined = typeof window === "undefined" ? null : window,
): boolean {
  const nav = navigatorLike as PrivacySignalSource | null | undefined;
  const win = windowLike as PrivacySignalSource | null | undefined;
  try {
    if (nav?.globalPrivacyControl === true) return true;
    return [nav?.doNotTrack, nav?.msDoNotTrack, win?.doNotTrack, win?.msDoNotTrack]
      .some((value) => typeof value === "string" && DO_NOT_TRACK_VALUES.includes(value));
  } catch {
    return false;
  }
}

export function privacySignalHeader(headers: Pick<Headers, "get">): boolean {
  return headers.get("sec-gpc")?.trim() === "1" || headers.get("dnt")?.trim() === "1";
}

export function clearUsageState(storage: StorageLike): void {
  for (const key of [COUNTERS_STORAGE_KEY, FUNNEL_STORAGE_KEY, INSTALL_STORAGE_KEY]) {
    try {
      storage.removeItem(key);
    } catch {}
  }
}

// --------------------------------------------------------------- install id

interface InstallRecord {
  id: string;
  createdAt: number;
}

/** The current install id, or a fresh one when missing, malformed, or older than 90 days. */
export function resolveInstallId(
  raw: string | null | undefined,
  now: number,
  randomUuid: () => string,
): { id: string; raw: string; rotated: boolean } {
  try {
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<InstallRecord>;
      if (
        isPlainObject(parsed) &&
        typeof parsed.id === "string" &&
        INSTALL_ID_RE.test(parsed.id) &&
        typeof parsed.createdAt === "number" &&
        now - parsed.createdAt >= 0 &&
        now - parsed.createdAt < INSTALL_ID_ROTATION_MS
      ) {
        return { id: parsed.id, raw, rotated: false };
      }
    }
  } catch {
    /* unreadable: rotate */
  }
  const id = randomUuid();
  const record: InstallRecord = { id, createdAt: now };
  return { id, raw: JSON.stringify(record), rotated: true };
}

// ----------------------------------------------------------------- counters

export function readCounters(raw: string | null | undefined): ProductCounters {
  const counters = emptyCounters();
  if (!raw) return counters;
  try {
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    if (!isPlainObject(parsed)) return counters;
    for (const field of PRODUCT_COUNTER_FIELDS) {
      const value = parsed[field];
      if (typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= U32_MAX) {
        counters[field] = value;
      }
    }
  } catch {
    /* unreadable: start from zero */
  }
  return counters;
}

export function hasCounts(counters: ProductCounters): boolean {
  return PRODUCT_COUNTER_FIELDS.some((field) => counters[field] > 0);
}

export function readFunnelCounters(raw: string | null | undefined): FunnelCounters {
  const counters = emptyFunnelCounters();
  if (!raw) return counters;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!isPlainObject(parsed)) return counters;
    for (const field of FUNNEL_COUNTER_FIELDS) {
      const value = parsed[field];
      if (typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= U32_MAX) counters[field] = value;
    }
  } catch {}
  return counters;
}

export function hasFunnelCounts(counters: FunnelCounters): boolean {
  return FUNNEL_COUNTER_FIELDS.some((field) => counters[field] > 0);
}

// ----------------------------------------------------------------- recorder

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface RecorderOptions {
  surface: Surface;
  appVersion: string;
  /** Same-origin route that forwards to the canonical ingest. */
  endpoint: string;
  storage: StorageLike;
  now?: () => number;
  randomUuid?: () => string;
  /** Transport; returns whether the batch was accepted. Never retried. */
  send?: (endpoint: string, body: string, signal?: AbortSignal) => Promise<boolean>;
  setTimer?: (callback: () => void, delayMs: number) => unknown;
  clearTimer?: (handle: unknown) => void;
  flushDelayMs?: number;
  blocked?: () => boolean;
}

export interface UsageRecorder {
  preference(): UsagePreference;
  /** Deliberately turn counting back on after an opt-out. */
  enable(): boolean;
  /** Record a durable opt-out and clear everything queued in this browser. */
  disable(): boolean;
  /** Re-read the preference from storage (another tab may have changed it). */
  sync(): void;
  record(counter: ProductCounter): void;
  recordFunnel(counter: FunnelCounter): void;
  pendingFunnel(): FunnelCounters;
  delivery(): "idle" | "accepted" | "rejected";
  /** Deliver whatever is queued now (also used on pagehide). */
  flush(): Promise<void>;
  pending(): ProductCounters;
}

function browserSend(endpoint: string, body: string, signal?: AbortSignal): Promise<boolean> {
  if (typeof fetch !== "function") return Promise.resolve(false);
  const controller = typeof AbortController === "function" ? new AbortController() : null;
  const timer = controller ? setTimeout(() => controller.abort(), 1500) : null;
  return fetch(endpoint, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body,
    keepalive: true,
    credentials: "omit",
    referrerPolicy: "no-referrer",
    signal: signal && controller ? AbortSignal.any([signal, controller.signal]) : signal ?? controller?.signal,
  })
    .then(async (response) => {
      if (!response.ok) return false;
      const bytes = await readBoundedBody(response, 512);
      const receipt: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
      return isPlainObject(receipt) && receipt.accepted === true;
    })
    .catch(() => false)
    .finally(() => {
      if (timer !== null) clearTimeout(timer);
    });
}

export function createUsageRecorder(options: RecorderOptions): UsageRecorder {
  const now = options.now ?? (() => Date.now());
  const randomUuid = options.randomUuid ?? (() => crypto.randomUUID());
  const send = options.send ?? browserSend;
  const setTimer = options.setTimer ?? ((callback, delay) => setTimeout(callback, delay));
  const clearTimer = options.clearTimer ?? ((handle) => clearTimeout(handle as ReturnType<typeof setTimeout>));
  const flushDelayMs = options.flushDelayMs ?? FLUSH_DELAY_MS;
  const { storage } = options;

  let counters = emptyCounters();
  let funnel = emptyFunnelCounters();
  let deliveryController: AbortController | null = null;
  let delivery: "idle" | "accepted" | "rejected" = "idle";
  let timer: unknown = null;
  let inFlight = false;
  let sessionOff = false;

  const read = (key: string) => {
    try {
      return storage.getItem(key);
    } catch {
      return null;
    }
  };
  const write = (key: string, value: string) => {
    try {
      storage.setItem(key, value);
      return storage.getItem(key) === value;
    } catch {
      return false;
    }
  };
  const remove = (key: string) => {
    try {
      storage.removeItem(key);
    } catch {
      /* nothing to clear */
    }
  };

  const cancelTimer = () => {
    if (timer !== null) clearTimer(timer);
    timer = null;
  };

  /** Everything queued and every identity goes; nothing pending survives. */
  const clearAll = () => {
    cancelTimer();
    deliveryController?.abort();
    counters = emptyCounters();
    funnel = emptyFunnelCounters();
    clearUsageState(storage);
  };

  const preference = (): UsagePreference => {
    if (sessionOff || options.blocked?.()) return "off";
    try { return readUsagePreference(storage.getItem(PREFERENCE_STORAGE_KEY)); } catch { return "off"; }
  };
  const enabled = () => usageCountingEnabled(preference());

  const schedule = () => {
    if (timer !== null) return;
    timer = setTimer(() => {
      timer = null;
      void flush();
    }, flushDelayMs);
  };

  const flush = async () => {
    if (inFlight) return;
    cancelTimer();
    if (!enabled()) {
      clearAll();
      return;
    }
    const batch = counters;
    const funnelBatch = funnel;
    if (!hasCounts(batch) && !hasFunnelCounts(funnelBatch)) return;
    const install = resolveInstallId(read(INSTALL_STORAGE_KEY), now(), randomUuid);
    if (install.rotated && !write(INSTALL_STORAGE_KEY, install.raw)) {
      delivery = "rejected";
      clearAll();
      return;
    }
    const envelope = buildEnvelope({
      counters: batch,
      funnel: funnelBatch,
      installId: install.id,
      appVersion: options.appVersion,
      surface: options.surface,
      now: now(),
    });
    const body = JSON.stringify(envelope);
    if (!validateEnvelope(envelope).ok || body.length > MAX_ENVELOPE_BYTES) return;
    // Discard after one attempt, accepted or not: there is no retry queue,
    // and a count that did not land is not worth remembering.
    counters = emptyCounters();
    funnel = emptyFunnelCounters();
    remove(COUNTERS_STORAGE_KEY);
    remove(FUNNEL_STORAGE_KEY);
    inFlight = true;
    deliveryController = new AbortController();
    try {
      delivery = await send(options.endpoint, body, deliveryController.signal) ? "accepted" : "rejected";
    } catch {
      delivery = "rejected";
    } finally {
      deliveryController = null;
      inFlight = false;
      if (enabled() && (hasCounts(counters) || hasFunnelCounts(funnel))) schedule();
    }
  };

  // Hydrate any counts a previous page on this origin left behind, but only
  // while counting is allowed; otherwise clear them as an opt-out's debris.
  if (enabled()) {
    counters = readCounters(read(COUNTERS_STORAGE_KEY));
    funnel = readFunnelCounters(read(FUNNEL_STORAGE_KEY));
  } else {
    clearAll();
  }

  return {
    preference,
    enable() {
      const saved = write(PREFERENCE_STORAGE_KEY, usagePreferenceRecord(true, now()));
      if (saved) sessionOff = false;
      return saved;
    },
    disable() {
      sessionOff = true;
      const saved = write(PREFERENCE_STORAGE_KEY, usagePreferenceRecord(false, now()));
      if (saved) sessionOff = false;
      clearAll();
      return saved;
    },
    sync() {
      if (!enabled()) clearAll();
    },
    record(counter) {
      if (!enabled()) {
        clearAll();
        return;
      }
      if (counters[counter] < U32_MAX) counters[counter] += 1;
      write(COUNTERS_STORAGE_KEY, JSON.stringify(counters));
      schedule();
    },
    recordFunnel(counter) {
      if (!enabled()) { clearAll(); return; }
      if (!(FUNNEL_COUNTER_FIELDS as readonly string[]).includes(counter)) return;
      if (funnel[counter] < U32_MAX) funnel[counter] += 1;
      write(FUNNEL_STORAGE_KEY, JSON.stringify(funnel));
      schedule();
    },
    pendingFunnel: () => ({ ...funnel }),
    delivery: () => delivery,
    flush,
    pending: () => ({ ...counters }),
  };
}
