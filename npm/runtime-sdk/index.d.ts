import type { RuntimeEventEnvelope, DynamicToolCallParams, DynamicToolCallResult } from "./generated/runtime.js";
export type { FleetProfileDocument, FleetProfileDocumentKind, ThreadLoadedListParams, ThreadLoadedListResponse, ThreadSetNameParams, ThreadSetNameResponse, RuntimeImageInput, RuntimeEventEnvelope, RuntimeCapabilities, RuntimeExperimentalCapabilities, DynamicToolSpec, DynamicToolSpecOutput, DynamicToolItemStatus, DynamicToolCallParams, DynamicToolCallResult, DynamicToolCallContent, TurnEnvironmentParams } from "./generated/runtime.js";

export type FleetRunId = string;
export type FleetRunStatus =
  | "pending"
  | "queued"
  | "running"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled";

export type FleetRuntimeTarget = "this_computer" | "another_computer" | "cloud";
export type FleetWorkflowKind = "parallel";

export interface FleetWorkflowDescriptor {
  id: string;
  kind: FleetWorkflowKind;
}

export type FleetWorkerStatus =
  | "unknown"
  | "online"
  | "busy"
  | "offline"
  | "unhealthy"
  | "draining"
  | "retired";

export type FleetArtifactKind =
  | "log"
  | "patch"
  | "test_result"
  | "report"
  | "checkpoint"
  | "receipt"
  | string;

export interface FleetStatusSummary {
  runs: number;
  queued: number;
  running: number;
  completed: number;
  partial: number;
  failed: number;
  restarted: number;
  escalated: number;
  transport_failed: number;
  task_failed: number;
  verifier_failed: number;
  cancelled: number;
  stale: number;
  workers: Record<string, FleetWorkerStatus>;
}

export interface FleetTaskStatusSummary {
  task_id: string;
  status: "enqueued" | "leased" | "completed" | "failed" | "cancelled";
  leased_to?: string | null;
  attempts: number;
}

export interface FleetRunSummary {
  id: string;
  name: string;
  lifecycle_status: FleetRunStatus;
  status: FleetStatusSummary;
  target?: FleetRuntimeTarget | null;
  workflow?: FleetWorkflowDescriptor | null;
  roles: string[];
  task_count: number;
  worker_count: number;
  tasks: FleetTaskStatusSummary[];
  labels: Record<string, string>;
  created_at: string;
  updated_at?: string | null;
  completed_at?: string | null;
}

export interface FleetRunDetail extends FleetRunSummary {
  task_specs: FleetTaskSpec[];
  worker_specs: FleetWorkerSpec[];
}

export interface FleetRunsResponse {
  status: FleetStatusSummary;
  runs: FleetRunSummary[];
}

export interface FleetTaskSpec {
  id: string;
  name: string;
  description?: string | null;
  objective?: string | null;
  instructions: string;
  worker?: FleetTaskWorkerProfile | null;
  workspace?: FleetWorkspaceRequirements | null;
  input_files?: string[];
  context?: string[];
  budget?: FleetTaskBudget | null;
  tags?: string[];
  expected_artifacts?: FleetArtifactKind[];
  scorer?: Record<string, unknown> | null;
  retry_policy?: Record<string, unknown> | null;
  alert_policy?: FleetAlertPolicy | null;
  timeout_seconds?: number | null;
  metadata?: Record<string, unknown>;
}

export interface FleetTaskWorkerProfile {
  agent_profile?: string | null;
  role?: string | null;
  loadout?: string | null;
  model_class?: string | null;
  model?: string | null;
  tool_profile?: string | null;
  tools?: string[];
  capabilities?: string[];
}

export interface FleetWorkspaceRequirements {
  root?: string | null;
  required_files?: string[];
  writable_paths?: string[];
  environment?: FleetEnvironmentRequirements | null;
}

export interface FleetEnvironmentRequirements {
  required?: string[];
  allowlist?: string[];
}

export interface FleetTaskBudget {
  max_tokens?: number | null;
  /** Maximum model turns. Omitted, null, or zero means unbounded. */
  max_steps?: number | null;
  /** Maximum admitted tool calls. Omitted, null, or zero means unbounded. */
  max_tool_calls?: number | null;
  max_seconds?: number | null;
}

export interface FleetAlertPolicy {
  events?: string[];
  channels?: Array<Record<string, unknown>>;
  after_attempts?: number | null;
  after_minutes_stale?: number | null;
}

export interface FleetWorkerSpec {
  id: string;
  name: string;
  host: Record<string, unknown>;
  labels?: Record<string, string>;
  capabilities?: string[];
  max_concurrent_tasks?: number | null;
}

export interface FleetArtifactRef {
  kind: FleetArtifactKind;
  path: string;
  checksum?: string | null;
  mime_type?: string | null;
  size_bytes?: number | null;
}

export type FleetWorkerEventPayload =
  | { state: "queued" }
  | { state: "leased"; lease_expires_at?: string | null }
  | { state: "starting" }
  | { state: "running" }
  | { state: "model_wait"; model?: string | null }
  | { state: "running_tool"; tool: string; call_id?: string | null }
  | { state: "heartbeat"; cpu_percent?: number | null; memory_mb?: number | null }
  | ({ state: "artifact" } & FleetArtifactRef)
  | { state: "completed"; exit_code?: number | null; summary?: string | null }
  | { state: "failed"; reason: string; recoverable?: boolean }
  | { state: "cancelled"; cancelled_by?: string | null }
  | { state: "interrupted"; signal?: string | null }
  | { state: "stale"; last_heartbeat_at?: string | null }
  | { state: "restarted"; restart_count?: number }
  | { state: "escalated"; channel: string; alert_id?: string | null };

export interface FleetWorkerEvent {
  seq: number;
  run_id: string;
  worker_id: string;
  task_id: string;
  timestamp: string;
  label?: string;
  payload: FleetWorkerEventPayload;
  extra?: Record<string, unknown>;
}

export interface FleetWorkerInspection {
  worker_id: string;
  status: FleetWorkerStatus;
  run_id?: string | null;
  task_id?: string | null;
  objective?: string | null;
  role?: string | null;
  host?: Record<string, unknown> | null;
  latest_heartbeat_at?: string | null;
  latest_event?: FleetWorkerEvent | null;
  artifacts: FleetArtifactRef[];
  last_error?: string | null;
  alert_state?: Record<string, unknown> | null;
}

export interface FleetWorkersResponse {
  run_id: string;
  workers: FleetWorkerInspection[];
}

export interface FleetWorkerActionResponse {
  action: "interrupt" | "restart" | "stop";
  worker: FleetWorkerInspection;
}

export interface StopFleetRunResponse {
  action: "stop";
  run_id: string;
  stopped: number;
  status: FleetStatusSummary;
}

export interface ManagedFleetRole {
  name: string;
  agent_profile?: string | null;
}

export interface ManagedFleetWorkflow extends FleetWorkflowDescriptor {
  tasks: FleetTaskSpec[];
}

export interface FleetRunCreateSpec {
  name?: string;
  target: FleetRuntimeTarget;
  roles: ManagedFleetRole[];
  workflow: ManagedFleetWorkflow;
  labels?: Record<string, string>;
  max_workers?: number;
}

export interface CreateFleetRunResponse {
  execution: "awaiting_start";
  run: FleetRunDetail;
  warnings: string[];
}

export interface StartFleetRunResponse {
  action: "start";
  execution: "scheduled";
  run_id: string;
  target: "this_computer";
  /** Leasing begins only after the scheduled driver owns the run. */
  leased: 0;
  queued: number;
  worker_ids: string[];
}

export interface FleetRuntimeEvent {
  cursor: string;
  event: string;
  run_id: string;
  worker_id?: string | null;
  task_id?: string | null;
  timestamp?: string | null;
  worker_seq?: number | null;
  payload: Record<string, unknown>;
}

export interface FleetStreamControlEvent {
  event:
    | "fleet.replay.truncated"
    | "fleet.replay.cursor_unavailable"
    | "fleet.stream.error";
  run_id?: string;
  cursor?: never;
  reload_projection?: boolean;
  retryable?: boolean;
}

export type FleetStreamEvent = FleetRuntimeEvent | FleetStreamControlEvent;

export interface FleetEventReplay {
  run_id: string;
  events: FleetRuntimeEvent[];
  has_more: boolean;
  history_truncated: boolean;
  next_cursor?: string | null;
}

export interface FleetEventOptions {
  after?: string;
  limit?: number;
}

export interface RuntimeClientOptions {
  baseUrl?: string;
  token?: string;
  fetch?: typeof fetch;
}

export interface ThreadRuntimeEvent extends Pick<RuntimeEventEnvelope, "seq" | "event" | "thread_id" | "timestamp"> {
  schema_version?: number;
  previous_seq?: number;
  turn_id?: string | null;
  item_id?: string | null;
  payload: Record<string, unknown>;
}

/** Transport progress at the existing journal cursor; never a new event. */
export interface ThreadStreamProgress {
  schema_version?: number;
  event: "stream.progress";
  kind?: "stream.progress";
  thread_id: string;
  seq: number;
  state: "replaying" | "live";
}

/**
 * The final frame whenever the Runtime ends a thread stream on purpose. It is
 * not a journal event (no `seq`). Resume with `sinceSeq: last_seq` when
 * `retryable`; stop and fall back to the thread snapshot otherwise. A stream
 * that ends without this frame lost its connection; resume from your cursor.
 */
export interface ThreadStreamEnd {
  schema_version: number;
  event: "stream.end";
  kind: "stream.end";
  thread_id: string;
  reason: "replay_failed" | "catch_up_failed" | "runtime_shutdown" | (string & {});
  last_seq: number;
  retryable: boolean;
}

/** Narrow an item yielded by threadEvents; journal event names are open-ended. */
export function isThreadStreamEnd(
  event: ThreadRuntimeEvent | ThreadStreamProgress | ThreadStreamEnd,
): event is ThreadStreamEnd;

export interface ThreadEventOptions {
  sinceSeq?: number;
  replayLimit?: number;
  signal?: AbortSignal;
}

export class RuntimeApiError extends Error {
  status?: number;
  method?: string;
  path?: string;
  body?: string;
}

export class RuntimeCapabilityError extends RuntimeApiError {
  capability: string;
}

export class CodeWhaleRuntimeClient {
  constructor(options?: RuntimeClientOptions);
  deliverDynamicToolResult(
    call: DynamicToolCallParams,
    result: DynamicToolCallResult,
    options?: { signal?: AbortSignal },
  ): Promise<void>;
  createFleetRun(spec: FleetRunCreateSpec): Promise<CreateFleetRunResponse>;
  startFleetRun(runId: FleetRunId): Promise<StartFleetRunResponse>;
  replayFleetEvents(runId: FleetRunId, options?: FleetEventOptions): Promise<FleetEventReplay>;
  listFleetRuns(): Promise<FleetRunsResponse>;
  getFleetRun(runId: FleetRunId): Promise<FleetRunDetail>;
  listFleetWorkers(runId: FleetRunId): Promise<FleetWorkersResponse>;
  getFleetWorker(workerId: string): Promise<FleetWorkerInspection>;
  interruptWorker(workerId: string): Promise<FleetWorkerActionResponse>;
  stopWorker(workerId: string): Promise<FleetWorkerActionResponse>;
  restartWorker(workerId: string): Promise<FleetWorkerActionResponse>;
  stopFleetRun(runId: FleetRunId): Promise<StopFleetRunResponse>;
  fleetEvents(
    runId: FleetRunId,
    options?: FleetEventOptions & { path?: string },
  ): AsyncIterable<FleetStreamEvent>;
  threadEvents(threadId: string, options: ThreadEventOptions & { includeProgress: true }): AsyncIterable<ThreadRuntimeEvent | ThreadStreamProgress | ThreadStreamEnd>;
  threadEvents(threadId: string, options?: ThreadEventOptions & { includeProgress?: false }): AsyncIterable<ThreadRuntimeEvent | ThreadStreamEnd>;
  threadEvents(threadId: string, options: ThreadEventOptions & { includeProgress: boolean }): AsyncIterable<ThreadRuntimeEvent | ThreadStreamProgress | ThreadStreamEnd>;
}

export function createRuntimeClient(options?: RuntimeClientOptions): CodeWhaleRuntimeClient;
