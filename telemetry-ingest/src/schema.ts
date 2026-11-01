/**
 * The published Codewhale telemetry schema, transcribed from `docs/TELEMETRY.md`
 * and enforced as a **closed** field set.
 *
 * The doc is the promise; this file is the enforcement. `test/schema-doc.test.ts`
 * parses the field names and enum spellings back out of `docs/TELEMETRY.md` and
 * asserts set equality against the constants below, so a doc that grows a field
 * this validator does not know — or a validator that grows a field the doc does
 * not publish — fails the build rather than quietly accepting new data.
 *
 * Why closed rather than "ignore what we don't recognise": a future client bug
 * that starts attaching a path, a prompt, or a provider table name must be
 * refused by the server, not stored. Unknown key anywhere in the batch rejects
 * the whole batch. There is no sanitising path — a payload the schema cannot
 * account for is not made safe by editing it, which is the same rule
 * `Event::is_bounded` applies on the client's drain path.
 */

/** `SCHEMA_VERSION` in `crates/telemetry/src/event.rs`. */
export const SCHEMA_VERSION = 4;

/** Notice-v5 batches without `model_usage`, `tool_usage` or the `ci` source. */
export const V3_SCHEMA_VERSION = 3;

/** Disclosed default-on policy, not a record of human acceptance. */
export const NOTICE_VERSION = 6;

/** The notice version schema v3 batches carry. */
export const V3_NOTICE_VERSION = 5;

/** Explicit consent to the PostHog processor disclosure. */
export const CONSENT_VERSION = 4;

/** `BATCH_MAX_EVENTS` in `crates/telemetry/src/actor.rs`. */
export const BATCH_MAX_EVENTS = 200;

/** `BATCH_MAX_BYTES` in `crates/telemetry/src/actor.rs` — the event budget. */
export const BATCH_MAX_BYTES = 64 * 1024;

/**
 * Hard body cap, computed rather than guessed.
 *
 * The client assembles at most `BATCH_MAX_EVENTS` (200) buffered lines totalling
 * at most `BATCH_MAX_BYTES` (65536) bytes — `actor::parse_events` breaks
 * *before* appending a line that would cross either bound, so both are ceilings,
 * and the batch is re-serialized by the same `serde` impl that wrote the lines,
 * so the event bytes on the wire equal the bytes on disk. On top of that the
 * envelope costs:
 *
 * - ~200 bytes of fixed keys and JSON punctuation,
 * - ~175 bytes of envelope values (`sent_at` 20, `install_id` 36,
 *   `app_version` <= 64, `git_sha` 12, and four short enums),
 * - 199 commas between the 200 events.
 *
 * 65536 + 200 + 175 + 199 = 66110 bytes is therefore the true maximum a
 * conforming client can send. 72 KiB leaves ~11% headroom for a future envelope
 * field without leaving room for a payload that is a different shape entirely.
 *
 * The disk rings (`buffer::MAX_EVENTS` = 512, `buffer::MAX_BYTES` = 256 KiB) are
 * larger, but they are the *storage* cap: a 512-record ring drains as three
 * batches, never as one 256 KiB POST.
 */
export const MAX_BODY_BYTES = 72 * 1024;

/**
 * Server-side length ceiling for the two version strings.
 *
 * `docs/TELEMETRY.md` pins their *shape*
 * (`^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$`) but not their length — a pre-release
 * suffix is unbounded in the published regex. INFERRED, not published: 64 bytes
 * is far past any real Cargo version and stops the one envelope field with an
 * open tail from becoming a free-form string slot.
 */
export const MAX_VERSION_LEN = 64;

/**
 * Server-side length ceiling for a reduced panic site. INFERRED: the published
 * rule is a charset, not a length. Real sites are well under 120 bytes.
 */
export const MAX_PANIC_SITE_LEN = 256;

/**
 * Server-side cardinality ceiling for `providers`. INFERRED: the published rule
 * closes the *value* space via `ProviderKind::as_str()`, not the array length.
 */
export const MAX_PROVIDERS = 32;

// ------------------------------------------------------------- enum spellings

/** `Surface::as_str` — the surface that produced the batch. */
export const SURFACES = [
  "tui",
  "exec",
  "cli",
  "app-server",
  "mcp-server",
  "serve",
  "website",
  "web-app",
  "desktop",
  "control-plane",
  // A VS Code-family extension client: deliberately generic, the way "web-app"
  // and "desktop" are. The editor's own name and version are not collected.
  // Appended, never inserted: `LEGACY_SURFACES` below is `slice(0, 6)`, and v1
  // must keep refusing every surface that was not in the runtime whitelist.
  "vscode-extension",
] as const;

const LEGACY_SURFACES = SURFACES.slice(0, 6);

/** `Os::as_str`. */
export const OSES = [
  "linux",
  "macos",
  "windows",
  "freebsd",
  "android",
  "other",
] as const;

/** `Arch::as_str`. */
export const ARCHES = ["x86_64", "aarch64", "other"] as const;

/** `Libc::as_str`. */
export const LIBCS = ["gnu", "musl", "none"] as const;

/** `InstallKind::as_str`. */
export const INSTALL_KINDS = ["install", "upgrade", "downgrade"] as const;

/** `SessionSource::as_str`. */
export const SESSION_SOURCES = [
  "interactive",
  "resume",
  "fork",
  "api",
  "unknown",
  "ci",
] as const;

/** `DurationBucket` wire spellings. */
export const DURATION_BUCKETS = [
  "lt_1m",
  "1m_10m",
  "10m_60m",
  "gt_60m",
] as const;

/** `ExitClass::as_str`. */
export const EXIT_CLASSES = ["clean", "signal", "panic", "error"] as const;

/** `ColdStartBucket` wire spellings. */
export const COLD_START_BUCKETS = [
  "lt_250",
  "250_1000",
  "1000_3000",
  "gte_3000",
] as const;

// ------------------------------------------------------------------ field sets

/** `Batch::FIELDS`, in declaration order. */
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

/** The unchanged v1 contract never establishes processor consent. */
export const LEGACY_ENVELOPE_FIELDS = ENVELOPE_FIELDS.filter((field) => field !== "notice_version");
/** V2 retains its original explicit-consent meaning and closed field set. */
export const V2_ENVELOPE_FIELDS = [...LEGACY_ENVELOPE_FIELDS, "consent_version"];

/** Aggregate product interactions; no page, account, session, or tool identifiers. */
export const PRODUCT_COUNTER_FIELDS = [
  "page_view", "docs_view", "install_copy", "download", "signup", "login",
  "session_create", "session_resume", "turn_submit", "turn_complete",
  "settings_open", "integration_connect", "error_shown",
] as const;

/** Anonymous operator-consented service health aggregates. */
export const OPERATIONS_FIELDS = [
  "requests", "errors", "duration_ms_total", "duration_ms_max", "probes", "probes_failed",
] as const;

/** `ModelCounters::FIELDS`; turn counts are u32, token counts u64. */
export const MODEL_COUNTER_FIELDS = [
  "turns_ok",
  "turns_failed",
  "turns_interrupted",
  "input_tokens",
  "output_tokens",
  "cache_read_tokens",
  "cache_write_tokens",
] as const;

/** `event::TOOL_NAMES` plus the `mcp` and `other` buckets. */
export const TOOL_NAMES = [
  "agent", "apply_patch", "authenticate", "bash", "code_execution", "create_goal",
  "diagnostics", "edit", "edit_file", "exec_shell", "execute_tools", "fetch_url",
  "file_search", "fim_edit", "finance", "get_goal", "git_blame", "git_commit_plan",
  "git_diff", "git_fetch", "git_log", "git_merge_tree", "git_show", "git_status",
  "grep_files", "handle_read", "image_ocr", "js_execution", "list_dir", "load_skill",
  "lsp", "memory_get", "memory_search", "note", "notify", "pandoc_convert",
  "project_map", "read", "read_file", "read_media", "remember", "request_plugin_install",
  "request_user_input", "retrieve_tool_result", "revert_turn", "review", "rlm",
  "run_tests", "run_verifiers", "session_get", "session_search", "start_mcp_server",
  "task_create", "task_shell_wait", "todo_write", "tool_search", "update_goal",
  "update_plan", "validate_data", "verify", "wait_for_dev_server", "web_search",
  "workflow", "write", "write_file", "mcp", "other",
] as const;

/** `Counters::FIELDS`, in declaration order. */
export const COUNTER_FIELDS = [
  "turns",
  "tool_calls",
  "fleet_dispatch",
  "workflow_run",
  "subagent_spawn",
  "mcp_server_connected",
  "memory_search",
  "approval_modal_shown",
  "approval_auto_allowed",
  "command_palette_open",
] as const;

/** `Errors::FIELDS`, in declaration order. */
export const ERROR_FIELDS = [
  "auth_preflight_failed",
  "provider_http_4xx",
  "provider_http_5xx",
  "tool_denied_by_policy",
  "tool_timeout",
  "network_error",
] as const;

/** `TurnWall::FIELDS`, in wire spelling and declaration order. */
export const TURN_WALL_FIELDS = [
  "lt_5s",
  "5_30s",
  "30_120s",
  "gte_120s",
] as const;

/**
 * Every event variant's complete key set, `event` tag included.
 *
 * `serde(tag = "event")` makes the wire form flat, so the tag is a key like any
 * other and the variant set is closed.
 */
export const EVENT_FIELDS: Readonly<Record<string, readonly string[]>> = {
  install_or_upgrade: ["event", "kind", "previous_version"],
  session_start: ["event", "source"],
  session_end: [
    "event",
    "duration_bucket",
    "exit_class",
    "cold_start_bucket",
    "providers",
    "counters",
    "errors",
    "turn_wall",
  ],
  panic: ["event", "site"],
  product_usage: ["event", "counters"],
  model_usage: ["event", "provider", "model", "counters", "turn_wall"],
  tool_usage: ["event", "tool", "calls", "errors"],
  operations_summary: ["event", ...OPERATIONS_FIELDS],
};

/** Every event discriminant, for the doc-match test. */
export const EVENT_NAMES = Object.keys(EVENT_FIELDS);

// -------------------------------------------------------------------- matchers

/** RFC3339 UTC at second precision — exactly `to_rfc3339_opts(Secs, true)`. */
const SENT_AT_RE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/;

/**
 * A canonical lowercase v4 UUID.
 *
 * `docs/TELEMETRY.md` and `envelope.rs` both say v4, and `Uuid::new_v4()` only
 * ever produces this form. The client's own read path accepts any parseable
 * UUID, so this is marginally stricter than the client — deliberately: an
 * `install_id.json` hand-written by something other than Codewhale is exactly
 * the input this endpoint should refuse, and refusing costs the user nothing
 * because the client drops rejected batches silently.
 */
const INSTALL_ID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/** `^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$`, plus the inferred length ceiling. */
const VERSION_RE = /^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/;

/** First 12 lowercase hex chars — `envelope::short_hex_sha`. */
const GIT_SHA_RE = /^[0-9a-f]{12}$/;

/** `event::is_reduced_panic_site` — the literal `<dep>`, or a `crates/…` site. */
const PANIC_SITE_RE = /^crates\/[A-Za-z0-9_/.-]+\.rs:\d+:\d+$/;

/**
 * `ProviderKind::as_str()` shape.
 *
 * This is the one field whose *value* space the endpoint cannot close: the
 * authoritative list is `codewhale_config::provider::all_providers()`, a Rust
 * registry with no generated artifact to read, and hard-coding a copy here
 * would drift into silently dropping a real user's route. The client closes it
 * with `Event::is_bounded` -> `is_known_provider_id` before the POST is made;
 * the server enforces the shape a closed `&'static str` enum can produce
 * (lowercase, hyphen-joined, bounded) and the doc's sorted-and-deduplicated
 * rule, which is what catches a client that started sending the customer's own
 * `[providers.<name>]` table key.
 */
const PROVIDER_RE = /^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$/;

const U32_MAX = 4294967295;

/**
 * `event::is_reported_model_id` shape: a lowercased bundled-catalog id or
 * `other`. The client closes the value against its catalog; the server bounds
 * the shape, as it does for providers.
 */
const MODEL_RE = /^[a-z0-9][a-z0-9._:/@+-]{0,127}$/;

/** Derived contract for CWC's product-only ingress; runtime events stay here. */
export const CWC_PRODUCT_SCHEMA = {
  $schema: "http://json-schema.org/draft-07/schema#",
  $id: "https://codewhale.net/schemas/cwc-product-telemetry-v3.json",
  title: "Codewhale CWC aggregate telemetry v3, notice v5",
  description: "Generated from telemetry-ingest/src/schema.ts; do not edit the JSON artifact. No content or identity fields.",
  type: "object",
  additionalProperties: false,
  required: ENVELOPE_FIELDS,
  properties: {
    schema_version: { const: V3_SCHEMA_VERSION },
    notice_version: { const: V3_NOTICE_VERSION },
    sent_at: { type: "string", pattern: SENT_AT_RE.source },
    install_id: { type: "string", pattern: INSTALL_ID_RE.source },
    app_version: { type: "string", maxLength: MAX_VERSION_LEN, pattern: VERSION_RE.source },
    git_sha: { const: null },
    surface: { enum: SURFACES.filter((surface) => surface === "web-app" || surface === "desktop") },
    os: { const: "other" },
    arch: { const: "other" },
    libc: { const: "none" },
    tty: { const: false },
    events: {
      type: "array",
      minItems: 1,
      maxItems: 1,
      items: {
        type: "object",
        additionalProperties: false,
        required: EVENT_FIELDS.product_usage,
        properties: {
          event: { const: "product_usage" },
          counters: {
            type: "object",
            additionalProperties: false,
            required: PRODUCT_COUNTER_FIELDS,
            properties: Object.fromEntries(PRODUCT_COUNTER_FIELDS.map((field) => [
              field, { type: "integer", minimum: 0, maximum: U32_MAX },
            ])),
          },
        },
      },
    },
  },
} as const;

// ------------------------------------------------------------------ validation

/** A rejection carries a reason for the tests. It is never sent to a client. */
export type Rejection = { ok: false; reason: string };

/** A batch that satisfies every published rule. */
export type Accepted = { ok: true; batch: Batch };

export type Counters = Record<(typeof COUNTER_FIELDS)[number], number>;
export type Errors = Record<(typeof ERROR_FIELDS)[number], number>;
export type TurnWall = Record<(typeof TURN_WALL_FIELDS)[number], number>;

export type ProductCounters = Record<(typeof PRODUCT_COUNTER_FIELDS)[number], number>;

export type Event =
  | { event: "install_or_upgrade"; kind: string; previous_version: string | null }
  | { event: "session_start"; source: string }
  | {
      event: "session_end";
      duration_bucket: string;
      exit_class: string;
      cold_start_bucket: string | null;
      providers: string[];
      counters: Counters;
      errors: Errors;
      turn_wall: TurnWall;
    }
  | { event: "panic"; site: string }
  | { event: "product_usage"; counters: ProductCounters }
  | {
      event: "model_usage";
      provider: string;
      model: string;
      counters: Record<(typeof MODEL_COUNTER_FIELDS)[number], number>;
      turn_wall: TurnWall;
    }
  | { event: "tool_usage"; tool: string; calls: number; errors: number }
  | ({ event: "operations_summary" } & Record<(typeof OPERATIONS_FIELDS)[number], number>);

export interface Batch {
  schema_version: number;
  consent_version?: number;
  notice_version?: number;
  sent_at: string;
  install_id: string;
  app_version: string;
  git_sha: string | null;
  surface: string;
  os: string;
  arch: string;
  libc: string;
  tty: boolean;
  events: Event[];
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return (
    typeof value === "object" && value !== null && !Array.isArray(value)
  );
}

/** Exact key-set equality. Missing keys and extra keys are both fatal. */
function keysExactly(
  value: Record<string, unknown>,
  expected: readonly string[],
  where: string,
): string | null {
  const actual = Object.keys(value);
  if (actual.length !== expected.length) {
    const extra = actual.filter((key) => !expected.includes(key));
    if (extra.length > 0) return `${where}: unexpected key ${extra[0]}`;
    const missing = expected.filter((key) => !actual.includes(key));
    return `${where}: missing key ${missing[0]}`;
  }
  for (const key of actual) {
    if (!expected.includes(key)) return `${where}: unexpected key ${key}`;
  }
  return null;
}

function enumString(
  value: unknown,
  allowed: readonly string[],
  where: string,
): string | null {
  if (typeof value !== "string" || !allowed.includes(value)) {
    return `${where}: not one of the documented values`;
  }
  return null;
}

function countMap(
  value: unknown,
  fields: readonly string[],
  where: string,
  max: number,
): string | null {
  if (!isPlainObject(value)) return `${where}: not an object`;
  const keyError = keysExactly(value, fields, where);
  if (keyError) return keyError;
  for (const field of fields) {
    const item = value[field];
    if (typeof item !== "number" || !Number.isInteger(item) || item < 0 || item > max) {
      return `${where}.${field}: not a count`;
    }
  }
  return null;
}

function u32Map(
  value: unknown,
  fields: readonly string[],
  where: string,
): string | null {
  if (!isPlainObject(value)) return `${where}: not an object`;
  const keyError = keysExactly(value, fields, where);
  if (keyError) return keyError;
  for (const field of fields) {
    const item = value[field];
    if (
      typeof item !== "number" ||
      !Number.isInteger(item) ||
      item < 0 ||
      item > U32_MAX
    ) {
      return `${where}.${field}: not a u32`;
    }
  }
  return null;
}

function validateEvent(value: unknown, where: string): string | null {
  if (!isPlainObject(value)) return `${where}: not an object`;
  const name = value.event;
  if (typeof name !== "string" || !Object.hasOwn(EVENT_FIELDS, name)) {
    return `${where}: unknown event discriminant`;
  }
  const keyError = keysExactly(value, EVENT_FIELDS[name], `${where}(${name})`);
  if (keyError) return keyError;

  switch (name) {
    case "install_or_upgrade": {
      const kindError = enumString(
        value.kind,
        INSTALL_KINDS,
        `${where}.kind`,
      );
      if (kindError) return kindError;
      const previous = value.previous_version;
      if (previous !== null) {
        if (
          typeof previous !== "string" ||
          previous.length > MAX_VERSION_LEN ||
          !VERSION_RE.test(previous)
        ) {
          return `${where}.previous_version: not a release version string`;
        }
      }
      return null;
    }
    case "session_start":
      return enumString(value.source, SESSION_SOURCES, `${where}.source`);
    case "session_end": {
      const bucketError = enumString(
        value.duration_bucket,
        DURATION_BUCKETS,
        `${where}.duration_bucket`,
      );
      if (bucketError) return bucketError;
      const exitError = enumString(
        value.exit_class,
        EXIT_CLASSES,
        `${where}.exit_class`,
      );
      if (exitError) return exitError;
      if (value.cold_start_bucket !== null) {
        const coldError = enumString(
          value.cold_start_bucket,
          COLD_START_BUCKETS,
          `${where}.cold_start_bucket`,
        );
        if (coldError) return coldError;
      }
      const providers = value.providers;
      if (!Array.isArray(providers)) return `${where}.providers: not an array`;
      if (providers.length > MAX_PROVIDERS) {
        return `${where}.providers: too many entries`;
      }
      let previous: string | null = null;
      for (const provider of providers) {
        if (typeof provider !== "string" || !PROVIDER_RE.test(provider)) {
          return `${where}.providers: not a provider id`;
        }
        // The doc says "sorted, deduplicated". A client that started shipping
        // the customer's own `[providers.<name>]` table key would land here
        // first, because an unsorted or repeated list is the cheapest signal
        // that this array stopped coming from `ProviderKind::as_str()`.
        if (previous !== null && provider <= previous) {
          return `${where}.providers: not sorted and deduplicated`;
        }
        previous = provider;
      }
      return (
        u32Map(value.counters, COUNTER_FIELDS, `${where}.counters`) ??
        u32Map(value.errors, ERROR_FIELDS, `${where}.errors`) ??
        u32Map(value.turn_wall, TURN_WALL_FIELDS, `${where}.turn_wall`)
      );
    }
    case "operations_summary": {
      const { event: _event, ...counts } = value;
      return u32Map(counts, OPERATIONS_FIELDS, where);
    }
    case "product_usage":
      return u32Map(value.counters, PRODUCT_COUNTER_FIELDS, `${where}.counters`);
    case "model_usage": {
      if (typeof value.provider !== "string" || !PROVIDER_RE.test(value.provider)) {
        return `${where}.provider: not a provider id`;
      }
      if (typeof value.model !== "string" || !MODEL_RE.test(value.model)) {
        return `${where}.model: not a catalogued model id`;
      }
      return (
        countMap(value.counters, MODEL_COUNTER_FIELDS, `${where}.counters`, Number.MAX_SAFE_INTEGER) ??
        u32Map(value.turn_wall, TURN_WALL_FIELDS, `${where}.turn_wall`)
      );
    }
    case "tool_usage":
      return (
        enumString(value.tool, TOOL_NAMES, `${where}.tool`) ??
        countMap({ calls: value.calls, errors: value.errors }, ["calls", "errors"], where, U32_MAX)
      );
    case "panic": {
      const site = value.site;
      if (typeof site !== "string" || site.length > MAX_PANIC_SITE_LEN) {
        return `${where}.site: not a string`;
      }
      if (site !== "<dep>" && !PANIC_SITE_RE.test(site)) {
        return `${where}.site: not a reduced panic site`;
      }
      return null;
    }
    default:
      return `${where}: unknown event discriminant`;
  }
}

/**
 * Validate one decoded batch against the published schema.
 *
 * Returns the batch on success. On failure the reason is for tests and local
 * reasoning only — the handler answers with a bare status and no body, because
 * echoing a parse error back is a way to learn what the endpoint stores.
 */
export function validateBatch(value: unknown): Accepted | Rejection {
  if (!isPlainObject(value)) return { ok: false, reason: "batch: not an object" };

  const legacy = value.schema_version === 1;
  const explicitConsent = value.schema_version === 2;
  const preUsage = value.schema_version !== SCHEMA_VERSION;
  if (
    !legacy &&
    !explicitConsent &&
    value.schema_version !== SCHEMA_VERSION &&
    value.schema_version !== V3_SCHEMA_VERSION
  ) {
    return { ok: false, reason: "batch.schema_version: unsupported" };
  }
  const fields = legacy ? LEGACY_ENVELOPE_FIELDS : explicitConsent ? V2_ENVELOPE_FIELDS : ENVELOPE_FIELDS;
  const keyError = keysExactly(value, fields, "batch");
  if (keyError) return { ok: false, reason: keyError };
  if (explicitConsent && value.consent_version !== CONSENT_VERSION) {
    return { ok: false, reason: "batch.consent_version: explicit current consent required" };
  }
  if (
    !legacy &&
    !explicitConsent &&
    value.notice_version !== (preUsage ? V3_NOTICE_VERSION : NOTICE_VERSION)
  ) {
    return { ok: false, reason: "batch.notice_version: current policy required" };
  }
  if (typeof value.sent_at !== "string" || !SENT_AT_RE.test(value.sent_at)) {
    return { ok: false, reason: "batch.sent_at: not RFC3339 UTC seconds" };
  }
  if (
    typeof value.install_id !== "string" ||
    !INSTALL_ID_RE.test(value.install_id)
  ) {
    return { ok: false, reason: "batch.install_id: not a v4 uuid" };
  }
  if (
    typeof value.app_version !== "string" ||
    value.app_version.length > MAX_VERSION_LEN ||
    !VERSION_RE.test(value.app_version)
  ) {
    return { ok: false, reason: "batch.app_version: not a release version" };
  }
  if (value.git_sha !== null) {
    if (typeof value.git_sha !== "string" || !GIT_SHA_RE.test(value.git_sha)) {
      return { ok: false, reason: "batch.git_sha: not 12 hex chars or null" };
    }
  }
  const enumErrors =
    enumString(value.surface, legacy ? LEGACY_SURFACES : SURFACES, "batch.surface") ??
    enumString(value.os, OSES, "batch.os") ??
    enumString(value.arch, ARCHES, "batch.arch") ??
    enumString(value.libc, LIBCS, "batch.libc");
  if (enumErrors) return { ok: false, reason: enumErrors };

  if (typeof value.tty !== "boolean") {
    return { ok: false, reason: "batch.tty: not a boolean" };
  }
  if (!Array.isArray(value.events)) {
    return { ok: false, reason: "batch.events: not an array" };
  }
  if (value.events.length > BATCH_MAX_EVENTS) {
    return { ok: false, reason: "batch.events: over BATCH_MAX_EVENTS" };
  }
  for (let index = 0; index < value.events.length; index += 1) {
    const event = value.events[index];
    if (isPlainObject(event)) {
      if (legacy && (event.event === "product_usage" || event.event === "operations_summary")) {
        return { ok: false, reason: "events: aggregate product/operations events require schema v2 or v3" };
      }
      if (preUsage && (event.event === "model_usage" || event.event === "tool_usage")) {
        return { ok: false, reason: "events: model_usage and tool_usage require schema v4" };
      }
      if (preUsage && event.event === "session_start" && event.source === "ci") {
        return { ok: false, reason: "events: the ci session source requires schema v4" };
      }
      if (event.event === "operations_summary" && value.surface !== "control-plane") {
        return { ok: false, reason: "events: operations_summary requires control-plane surface" };
      }
    }
    const eventError = validateEvent(event, `events[${index}]`);
    if (eventError) return { ok: false, reason: eventError };
  }

  return { ok: true, batch: value as unknown as Batch };
}
