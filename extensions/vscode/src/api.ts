/**
 * Codewhale Runtime HTTP/SSE client — the `/v1` contract documented in
 * `docs/RUNTIME_API.md`.
 *
 * This module is deliberately VS Code-free so it can be unit-tested with
 * plain node. Callers pass the base URL and token explicitly; the VS Code
 * side of token resolution lives in `secrets.ts` / `runtime.ts`.
 */
import * as http from "node:http";
import { SseParser, type RuntimeEvent } from "./sse";

export type { RuntimeEvent };

export interface ApiConfig {
  baseUrl: string;
  token?: string;
}

export interface ThreadRecord {
  id: string;
  title?: string;
  model?: string;
  modelProvider?: string;
  workspace?: string;
  mode?: string;
  latestTurnId?: string;
  archived: boolean;
  updatedAt: string;
}

export interface ItemRecord {
  id: string;
  turnId?: string;
  kind: string;
  status?: string;
  summary: string;
  detail?: string;
  metadata?: Record<string, unknown>;
  startedAt?: string;
  endedAt?: string;
}

export interface TurnRecord {
  id: string;
  threadId?: string;
  status?: string;
  effectiveModel?: string;
  error?: string;
}

export interface PendingApproval {
  id: string;
  turnId?: string;
  toolName: string;
  description: string;
  intentSummary?: string;
}

export interface UserInputOption {
  label: string;
  description?: string;
}

export interface UserInputQuestion {
  header?: string;
  id: string;
  question: string;
  options: UserInputOption[];
  allowFreeText?: boolean;
  multiSelect?: boolean;
}

export interface PendingUserInput {
  id: string;
  turnId?: string;
  questions: UserInputQuestion[];
}

export interface ThreadSummary {
  id: string;
  title: string;
  preview: string;
  model: string;
  mode: string;
  workspace?: string;
  branch?: string;
  head?: string;
  dirty: boolean;
  archived: boolean;
  updatedAt: string;
  latestTurnStatus?: string;
}

export interface SnapshotEntry {
  id: string;
  label: string;
  timestamp: number;
}

export interface ThreadDetail {
  thread: ThreadRecord;
  turns: TurnRecord[];
  items: ItemRecord[];
  latestSeq: number;
  pendingApprovals: PendingApproval[];
  pendingUserInputs: PendingUserInput[];
}

export interface StartTurnResult {
  thread: ThreadRecord;
  turn: TurnRecord;
}

export interface ConnectionInfo {
  kind: "connected" | "offline" | "auth-required" | "error";
  detail: string;
  version?: string;
}

const HEALTH_TIMEOUT_MS = 2500;
const READ_TIMEOUT_MS = 8000;
const MUTATE_TIMEOUT_MS = 20000;

export async function checkConnection(config: ApiConfig): Promise<ConnectionInfo> {
  const health = await requestJson(`${config.baseUrl}/health`, config, {
    timeoutMs: HEALTH_TIMEOUT_MS,
  });
  if (health.statusCode === 0) {
    return { kind: "offline", detail: "Runtime is not reachable." };
  }
  if (health.statusCode === 401) {
    return { kind: "auth-required", detail: "Runtime requires a token." };
  }
  if (!isOk(health.statusCode)) {
    return { kind: "error", detail: `Health check returned HTTP ${health.statusCode}.` };
  }

  const info = await requestJson(`${config.baseUrl}/v1/runtime/info`, config, {
    timeoutMs: HEALTH_TIMEOUT_MS,
  });
  if (info.statusCode === 401) {
    return { kind: "auth-required", detail: "Runtime info requires a token." };
  }

  // `/health` and `/v1/runtime/info` are intentionally unauthenticated, so a
  // token-protected runtime answers both with HTTP 200. The info body carries
  // the real signal: `auth_required`.
  if (readBoolean(readBody(info.body).auth_required) && !config.token) {
    return {
      kind: "auth-required",
      detail: "Runtime requires a bearer token. Store one with CodeWhale: Set Runtime Token.",
    };
  }

  const version = readString(readBody(info.body).version);
  return {
    kind: "connected",
    detail: version ? `Connected to CodeWhale ${version}.` : "Connected to CodeWhale runtime.",
    version,
  };
}

export async function listThreadSummaries(config: ApiConfig, limit = 20): Promise<ThreadSummary[]> {
  const response = await requestJson(
    `${config.baseUrl}/v1/threads/summary?limit=${encodeURIComponent(String(limit))}`,
    config,
    { timeoutMs: READ_TIMEOUT_MS },
  );
  ensureOk(response, "Thread summaries");
  return readThreadSummaries(response.body);
}

export async function getThreadDetail(config: ApiConfig, threadId: string): Promise<ThreadDetail> {
  const response = await requestJson(
    `${config.baseUrl}/v1/threads/${encodeURIComponent(threadId)}`,
    config,
    { timeoutMs: READ_TIMEOUT_MS },
  );
  ensureOk(response, "Thread detail");
  return readThreadDetail(response.body);
}

export async function createThread(
  config: ApiConfig,
  body: { workspace?: string; model?: string; mode?: string } = {},
): Promise<ThreadRecord> {
  const response = await requestJson(`${config.baseUrl}/v1/threads`, config, {
    method: "POST",
    body: JSON.stringify(body),
    timeoutMs: MUTATE_TIMEOUT_MS,
  });
  ensureOk(response, "Create thread");
  return readThread(response.body);
}

export interface StartTurnBody {
  prompt: string;
  operationKey?: string;
}

export async function startTurn(
  config: ApiConfig,
  threadId: string,
  body: StartTurnBody,
): Promise<StartTurnResult> {
  const wire: Record<string, unknown> = { prompt: body.prompt };
  if (body.operationKey) {
    wire.operation_key = body.operationKey;
  }
  const response = await requestJson(
    `${config.baseUrl}/v1/threads/${encodeURIComponent(threadId)}/turns`,
    config,
    { method: "POST", body: JSON.stringify(wire), timeoutMs: MUTATE_TIMEOUT_MS },
  );
  ensureOk(response, "Start turn");
  const record = readBody(response.body);
  return {
    thread: readThread(record.thread),
    turn: readTurn(record.turn),
  };
}

export async function steerTurn(
  config: ApiConfig,
  threadId: string,
  turnId: string,
  prompt: string,
): Promise<void> {
  const response = await requestJson(
    `${config.baseUrl}/v1/threads/${encodeURIComponent(threadId)}/turns/${encodeURIComponent(turnId)}/steer`,
    config,
    { method: "POST", body: JSON.stringify({ prompt }), timeoutMs: MUTATE_TIMEOUT_MS },
  );
  ensureOk(response, "Steer");
}

/**
 * Outcome of an interrupt. The runtime answers 409 when the turn is not
 * running — that is "nothing to stop", not a failure, so it is reported as a
 * value instead of thrown.
 */
export type InterruptResult = "interrupted" | "not-running";

export async function interruptTurn(
  config: ApiConfig,
  threadId: string,
  turnId: string,
): Promise<InterruptResult> {
  const response = await requestJson(
    `${config.baseUrl}/v1/threads/${encodeURIComponent(threadId)}/turns/${encodeURIComponent(turnId)}/interrupt`,
    config,
    { method: "POST", timeoutMs: MUTATE_TIMEOUT_MS },
  );
  if (response.statusCode === 409) {
    return "not-running";
  }
  ensureOk(response, "Interrupt");
  return "interrupted";
}

export async function resolveApproval(
  config: ApiConfig,
  approvalId: string,
  decision: "allow" | "deny",
  remember = false,
): Promise<void> {
  const response = await requestJson(
    `${config.baseUrl}/v1/approvals/${encodeURIComponent(approvalId)}`,
    config,
    {
      method: "POST",
      body: JSON.stringify({ decision, remember }),
      timeoutMs: MUTATE_TIMEOUT_MS,
    },
  );
  ensureOk(response, "Approval");
}

export async function answerUserInput(
  config: ApiConfig,
  threadId: string,
  inputId: string,
  answers: Array<{ id: string; label: string; value: string }>,
): Promise<void> {
  const response = await requestJson(
    `${config.baseUrl}/v1/user-input/${encodeURIComponent(threadId)}/${encodeURIComponent(inputId)}`,
    config,
    { method: "POST", body: JSON.stringify({ answers }), timeoutMs: MUTATE_TIMEOUT_MS },
  );
  ensureOk(response, "User input");
}

export async function listSnapshots(config: ApiConfig, limit = 8): Promise<SnapshotEntry[]> {
  const response = await requestJson(
    `${config.baseUrl}/v1/snapshots?limit=${encodeURIComponent(String(limit))}`,
    config,
    { timeoutMs: READ_TIMEOUT_MS },
  );
  ensureOk(response, "Restore points");
  return readSnapshots(response.body);
}

export interface EventStream {
  readonly threadId: string;
  onEvent: (event: RuntimeEvent) => void;
  onError: (error: Error) => void;
  close(): void;
}

/**
 * Open the replay + live SSE stream for a thread. `sinceSeq` should be the
 * last accepted per-thread sequence (0 for a fresh thread). The stream never
 * reconnects on its own; callers decide the retry policy from `onError`.
 */
export function openEventStream(
  config: ApiConfig,
  threadId: string,
  sinceSeq: number,
  parser = new SseParser(),
): EventStream {
  const url = `${config.baseUrl}/v1/threads/${encodeURIComponent(threadId)}/events?since_seq=${String(sinceSeq)}`;
  const request = http.get(
    url,
    {
      headers: {
        Accept: "text/event-stream",
        ...(config.token ? { Authorization: `Bearer ${config.token}` } : {}),
      },
    },
    (response) => {
      if (!isOk(response.statusCode ?? 0)) {
        response.resume();
        stream.onError(apiError(response.statusCode ?? 0, undefined, "Event stream"));
        return;
      }
      response.setEncoding("utf8");
      response.on("data", (chunk: string) => {
        for (const event of parser.push(chunk)) {
          stream.onEvent(event);
        }
      });
      response.on("end", () => {
        stream.onError(new Error("Event stream closed."));
      });
      response.on("error", (error: Error) => {
        stream.onError(error);
      });
    },
  );
  request.on("error", (error: Error) => {
    stream.onError(error);
  });

  const stream: EventStream = {
    threadId,
    onEvent: () => undefined,
    onError: () => undefined,
    close: () => {
      request.destroy();
    },
  };
  return stream;
}

export class ApiError extends Error {
  readonly statusCode: number;
  readonly detail?: string;

  constructor(message: string, statusCode: number, detail?: string) {
    super(detail ? `${message} ${detail}` : message);
    this.name = "ApiError";
    this.statusCode = statusCode;
    this.detail = detail;
  }
}

/**
 * HTTP 409 from the runtime: the request was well-formed but the resource is
 * already in a state that refuses it — most often "thread already has an
 * active turn". Callers catch this to say "a turn is already running" instead
 * of reporting a generic failure.
 */
export class ConflictError extends ApiError {
  constructor(message: string, detail?: string) {
    super(message, 409, detail);
    this.name = "ConflictError";
  }
}

interface RequestResult {
  statusCode: number;
  body: unknown;
}

/**
 * Success is the whole 2xx range, mirroring `response.ok` in the embedded web
 * client (`crates/app-server/src/runtime_web/app.mjs:873`). The runtime answers 201
 * CREATED for `POST /v1/threads/{id}/turns`, so an equality check against a
 * hand-picked list of codes rejects every real send.
 */
function isOk(statusCode: number): boolean {
  return statusCode >= 200 && statusCode < 300;
}

/** Build the typed error for a non-2xx status, surfacing the runtime's own message. */
function apiError(statusCode: number, body: unknown, label: string): ApiError {
  const detail = readErrorDetail(body);
  if (statusCode === 0) {
    return new ApiError(`${label} could not reach the runtime.`, 0, detail);
  }
  if (statusCode === 401) {
    return new ApiError(`${label} requires the runtime token.`, 401, detail);
  }
  if (statusCode === 409) {
    return new ConflictError(`${label} conflicts with the runtime's current state.`, detail);
  }
  return new ApiError(`${label} returned HTTP ${statusCode}.`, statusCode, detail);
}

/** Throw unless the runtime answered 2xx. Every route's status check runs through here. */
function ensureOk(response: RequestResult, label: string): void {
  if (!isOk(response.statusCode)) {
    throw apiError(response.statusCode, response.body, label);
  }
}

async function requestJson(
  url: string,
  config: ApiConfig,
  options: { method?: string; body?: string; timeoutMs: number },
): Promise<RequestResult> {
  try {
    return await new Promise<RequestResult>((resolve, reject) => {
      const request = http.request(
        url,
        {
          method: options.method ?? "GET",
          timeout: options.timeoutMs,
          headers: {
            Accept: "application/json",
            ...(options.body ? { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(options.body) } : {}),
            ...(config.token ? { Authorization: `Bearer ${config.token}` } : {}),
          },
        },
        (response) => {
          let raw = "";
          response.setEncoding("utf8");
          response.on("data", (chunk: string) => {
            raw += chunk;
          });
          response.on("end", () => {
            resolve({ statusCode: response.statusCode ?? 0, body: parseJson(raw) });
          });
        },
      );
      if (options.body) {
        request.write(options.body);
      }
      request.on("timeout", () => {
        request.destroy(new Error("Runtime request timed out."));
      });
      request.on("error", reject);
      request.end();
    });
  } catch (error: unknown) {
    const detail = error instanceof Error ? error.message : String(error);
    return { statusCode: 0, body: { error: detail } };
  }
}

function parseJson(raw: string): unknown {
  try {
    return JSON.parse(raw);
  } catch {
    return undefined;
  }
}

function readBody(body: unknown): Record<string, unknown> {
  return body && typeof body === "object" ? (body as Record<string, unknown>) : {};
}

function readErrorDetail(body: unknown): string | undefined {
  const record = readBody(body);
  const error = record.error;
  if (typeof error === "string") {
    return error;
  }
  if (error && typeof error === "object") {
    const message = readBody(error).message ?? readBody(error).code;
    if (typeof message === "string") {
      return message;
    }
  }
  return undefined;
}

function readString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function readNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function readBoolean(value: unknown): boolean {
  return value === true;
}

function readThread(value: unknown): ThreadRecord {
  const record = readBody(value);
  return {
    id: readString(record.id) ?? "",
    title: readString(record.title),
    model: readString(record.model),
    modelProvider: readString(record.model_provider),
    workspace: readString(record.workspace),
    mode: readString(record.mode),
    latestTurnId: readString(record.latest_turn_id),
    archived: record.archived === true,
    updatedAt: readString(record.updated_at) ?? "",
  };
}

function readTurn(value: unknown): TurnRecord {
  const record = readBody(value);
  return {
    id: readString(record.id) ?? "",
    threadId: readString(record.thread_id),
    status: readString(record.status),
    effectiveModel: readString(record.effective_model),
    error: readString(record.error),
  };
}

function readItem(value: unknown): ItemRecord | undefined {
  const record = readBody(value);
  const id = readString(record.id);
  if (!id) {
    return undefined;
  }
  return {
    id,
    turnId: readString(record.turn_id),
    kind: readString(record.kind) ?? "status",
    status: readString(record.status),
    summary: readString(record.summary) ?? "",
    detail: readString(record.detail),
    metadata:
      record.metadata && typeof record.metadata === "object"
        ? (record.metadata as Record<string, unknown>)
        : undefined,
    startedAt: readString(record.started_at),
    endedAt: readString(record.ended_at),
  };
}

function readThreadDetail(value: unknown): ThreadDetail {
  const record = readBody(value);
  const thread = readThread(record.thread);
  const items = Array.isArray(record.items)
    ? record.items.flatMap((item) => {
        const parsed = readItem(item);
        return parsed ? [parsed] : [];
      })
    : [];
  const turns = Array.isArray(record.turns)
    ? record.turns.flatMap((turn) => {
        const parsed = readTurn(turn);
        return parsed.id ? [parsed] : [];
      })
    : [];

  const pendingApprovals = Array.isArray(record.pending_approvals)
    ? record.pending_approvals.flatMap((entry) => {
        const approval = readBody(entry);
        const id = readString(approval.id);
        if (!id) {
          return [];
        }
        return [
          {
            id,
            turnId: readString(approval.turn_id),
            toolName: readString(approval.tool_name) ?? "tool",
            description: readString(approval.description) ?? "",
            intentSummary: readString(approval.intent_summary),
          },
        ];
      })
    : [];

  const pendingUserInputs = Array.isArray(record.pending_user_inputs)
    ? record.pending_user_inputs.flatMap((entry) => {
        const input = readBody(entry);
        const id = readString(input.id);
        const request = readBody(input.request);
        const questions = Array.isArray(request.questions)
          ? request.questions.flatMap((raw) => {
              const question = readBody(raw);
              const questionId = readString(question.id);
              if (!questionId) {
                return [];
              }
              return [
                {
                  header: readString(question.header),
                  id: questionId,
                  question: readString(question.question) ?? "",
                  allowFreeText: question.allow_free_text === true,
                  multiSelect: question.multi_select === true,
                  options: Array.isArray(question.options)
                    ? question.options.flatMap((option) => {
                        const recordOption = readBody(option);
                        const label = readString(recordOption.label);
                        return label ? [{ label, description: readString(recordOption.description) }] : [];
                      })
                    : [],
                },
              ];
            })
          : [];
        if (!id) {
          return [];
        }
        return [{ id, turnId: readString(input.turn_id), questions }];
      })
    : [];

  return {
    thread,
    turns,
    items,
    latestSeq: readNumber(record.latest_seq) ?? 0,
    pendingApprovals,
    pendingUserInputs,
  };
}

function readThreadSummaries(value: unknown): ThreadSummary[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.flatMap((item) => {
    const record = readBody(item);
    const id = readString(record.id);
    if (!id) {
      return [];
    }
    return [
      {
        id,
        title: readString(record.title) ?? "New Thread",
        preview: readString(record.preview) ?? "",
        model: readString(record.model) ?? "unknown",
        mode: readString(record.mode) ?? "agent",
        workspace: readString(record.workspace),
        branch: readString(record.branch),
        head: readString(record.head),
        dirty: record.dirty === true,
        archived: record.archived === true,
        updatedAt: readString(record.updated_at) ?? "",
        latestTurnStatus: readString(record.latest_turn_status),
      },
    ];
  });
}

function readSnapshots(value: unknown): SnapshotEntry[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.flatMap((item) => {
    const record = readBody(item);
    const id = readString(record.id);
    const label = readString(record.label);
    const timestamp = readNumber(record.timestamp);
    if (!id || !label || timestamp === undefined) {
      return [];
    }
    return [{ id, label, timestamp }];
  });
}
