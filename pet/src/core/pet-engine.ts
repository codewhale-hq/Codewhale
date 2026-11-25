import { PET_MAX_SECONDS } from './pet-sim.js';
import type { Category, WhaleEvent } from './model.js';
import { compilePetTelemetry, PET_BIN_MS, type PetBucket } from './pet-telemetry.js';

// Engine emits a liveness pulse every 10 seconds for a running operation.
// Keep a small scheduling margin so one delayed pulse does not erase a valid
// long operation from the owner projection; after that, it becomes generic.
// The physics tape keeps its own, shorter coverage rule (see `pulse`).
export const ENGINE_OWNER_STALE_MS = 12_000;
export type OwnerActivityKind = 'reading' | 'editing' | 'searching' | 'testing' | 'executing'
  | 'browsing' | 'computer' | 'memory' | 'tool' | 'thinking' | 'responding' | 'delegating';
export type OwnerPresence = 'unknown' | 'working' | 'needs_you' | 'done' | 'idle';
export type OwnerFreshness = 'missing' | 'fresh' | 'stale';
export type OwnerTurnOutcome = 'completed' | 'interrupted' | 'failed';
export type OwnerOperationOutcome = 'succeeded' | 'failed' | 'cancelled' | 'denied';

export interface EngineOwnerProjection {
  schemaVersion: 1;
  /** Added by the existing owner after selecting its session source. */
  sessionId: string | null;
  /** Added by the existing owner; monotonic for this session source. */
  cursor: number;
  observed: boolean;
  freshness: OwnerFreshness;
  authoritativePresence: OwnerPresence;
  activityKind: OwnerActivityKind | null;
  actionId?: string;
  observedAtMs: number | null;
  parallelAgentCount: number;
  activeSpans: { activityKind: OwnerActivityKind; actionId?: string; startedAtMs: number }[];
  turnId: string | null;
  turnOutcome: OwnerTurnOutcome | null;
  doneEffectId: string | null;
  failedToolAge: { activityKind: OwnerActivityKind; actionId?: string; ageMs: number } | null;
}

type SpanGroup = 'operation' | 'agent' | 'thinking' | 'responding';
interface ActiveSpan {
  key: string;
  activityKind: OwnerActivityKind;
  actionId?: string;
  startedAtMs: number;
  event: WhaleEvent;
  group: SpanGroup;
}
interface FailedTool {
  activityKind: OwnerActivityKind;
  actionId?: string;
  failedAtMs: number;
}

const ACTIVITY_KINDS: readonly OwnerActivityKind[] = [
  'reading', 'editing', 'searching', 'testing', 'executing', 'browsing', 'computer',
  'memory', 'tool', 'thinking', 'responding', 'delegating',
];
const TURN_OUTCOMES: readonly OwnerTurnOutcome[] = ['completed', 'interrupted', 'failed'];
const OPERATION_OUTCOMES: readonly OwnerOperationOutcome[] = ['succeeded', 'failed', 'cancelled', 'denied'];
const APPROVAL_OUTCOMES = ['approved', 'denied', 'cancelled'] as const;

function categoryFor(kind: OwnerActivityKind): Category {
  switch (kind) {
    case 'reading': case 'editing': case 'searching': return 'filesystem';
    case 'testing': case 'executing': return 'code';
    case 'browsing': return 'browser';
    case 'computer': case 'tool': return 'tool';
    case 'memory': return 'memory';
    case 'thinking': return 'reasoning';
    case 'responding': return 'communication';
    case 'delegating': return 'agent';
  }
}

/** Read-only reducer for the Engine owner's typed metadata. It keeps using the
 * existing pet telemetry tape and physics owner; it neither recognizes tool
 * names from content nor accepts transcript text, arguments, commands, or results. */
export class PetEngineTelemetry {
  private events: WhaleEvent[] = [];
  private active = new Map<string, ActiveSpan>();
  private waiting: WhaleEvent | undefined;
  private sequence = 0;
  private lastTime = 0;
  private lastObservedAt: number | undefined;
  private turnId: string | undefined;
  private turnOutcome: OwnerTurnOutcome | undefined;
  private terminalAt: number | undefined;
  private lastFailedTool: FailedTool | undefined;
  private completedSpans = new Set<string>();
  private completedTurns = new Set<string>();

  /** Ephemeral safe read projection. Span ids are retained only in this
   * reducer to correlate trusted lifecycle events and never leave the owner. */
  activity(at: number): EngineOwnerProjection {
    const observed = this.lastObservedAt !== undefined;
    const freshness: OwnerFreshness = !observed ? 'missing'
      : at - this.lastObservedAt! <= ENGINE_OWNER_STALE_MS ? 'fresh' : 'stale';
    const fresh = freshness === 'fresh';
    const active = fresh
      ? [...this.active.values()]
        .filter(span => at >= span.startedAtMs && at - span.event.endTime <= ENGINE_OWNER_STALE_MS)
        // The parent's own work leads; delegated agents are counted in
        // `parallelAgentCount` and only lead when nothing else is active.
        .sort((a, b) => Number(a.group === 'agent') - Number(b.group === 'agent')
          || b.startedAtMs - a.startedAtMs || a.key.localeCompare(b.key))
      : [];
    const agents = active.filter(span => span.group === 'agent').length;
    const terminalFresh = fresh && this.terminalAt !== undefined
      && at >= this.terminalAt && at - this.terminalAt <= ENGINE_OWNER_STALE_MS;
    let authoritativePresence: OwnerPresence = 'unknown';
    if (fresh) {
      if (this.waiting) authoritativePresence = 'needs_you';
      else if (terminalFresh && this.turnOutcome === 'completed' && this.turnId) authoritativePresence = 'done';
      else if (terminalFresh) authoritativePresence = 'idle';
      else if (active.length > 0 || this.turnId) authoritativePresence = 'working';
    }
    const activityKind = fresh && authoritativePresence !== 'needs_you'
      && authoritativePresence !== 'done' && authoritativePresence !== 'idle'
      ? active[0]?.activityKind
        ?? (this.lastFailedTool && at >= this.lastFailedTool.failedAtMs
          && at - this.lastFailedTool.failedAtMs <= ENGINE_OWNER_STALE_MS
          ? this.lastFailedTool.activityKind : null)
      : null;
    const failedAge = fresh && this.lastFailedTool
      && at >= this.lastFailedTool.failedAtMs
      && at - this.lastFailedTool.failedAtMs <= ENGINE_OWNER_STALE_MS
      ? { activityKind: this.lastFailedTool.activityKind,
        ...(this.lastFailedTool.actionId ? { actionId: this.lastFailedTool.actionId } : {}),
        ageMs: at - this.lastFailedTool.failedAtMs }
      : null;
    const actionId = activityKind ? (active.length ? active[0].actionId : failedAge?.actionId) : undefined;
    const doneEffectId = authoritativePresence === 'done' && terminalFresh
      && this.turnOutcome === 'completed' && this.turnId ? this.turnId : null;

    return {
      schemaVersion: 1,
      sessionId: null,
      cursor: 0,
      observed,
      freshness,
      authoritativePresence,
      activityKind: activityKind ?? null,
      ...(actionId ? { actionId } : {}),
      observedAtMs: this.lastObservedAt ?? null,
      parallelAgentCount: fresh ? agents : 0,
      activeSpans: active.slice(0, 4).map(span => ({
        activityKind: span.activityKind,
        ...(span.actionId ? { actionId: span.actionId } : {}),
        startedAtMs: span.startedAtMs,
      })),
      turnId: this.turnId ?? null,
      turnOutcome: this.turnOutcome ?? null,
      doneEffectId,
      failedToolAge: failedAge,
    };
  }

  private add(name: string, category: Category, at: number, agentId = 'parent', continuation = false): WhaleEvent {
    if (this.events.length >= 8192) throw new Error('Pet Engine observation window is full.');
    const event: WhaleEvent = {
      schemaVersion: 1,
      id: `engine:${this.sequence++}`,
      traceId: 'foreground',
      startTime: at,
      endTime: at,
      name,
      category,
      agentId,
      status: 'running',
      attributes: continuation ? { 'whalesong.continuation': true } : {},
    };
    this.events.push(event);
    return event;
  }

  private start(key: string, kind: OwnerActivityKind, group: SpanGroup, at: number, agentId = 'parent', actionId?: string): void {
    if (this.active.has(key)) return;
    if (this.active.size >= 256) throw new Error('Too many active Engine pet spans.');
    const event = this.add(group === 'agent' ? 'agent' : group === 'thinking' ? 'thinking'
      : group === 'responding' ? 'assistant_message' : 'operation', categoryFor(kind), at, agentId);
    this.active.set(key, { key, activityKind: kind, actionId, startedAtMs: at, event, group });
  }

  private pulse(key: string, at: number): void {
    const span = this.active.get(key);
    if (!span) return;
    // A resumed stream does not assert tape coverage across its silent
    // interval; the span itself (and its start) stays active.
    if (at - span.event.endTime > PET_BIN_MS * 2) {
      const event = this.add(span.event.name, span.event.category, at, span.event.agentId, true);
      this.active.set(key, { ...span, event });
    } else {
      span.event.endTime = at;
    }
  }

  private finish(key: string, at: number, status: 'success' | 'unknown' = 'success'): ActiveSpan | undefined {
    if (!this.active.has(key)) return undefined;
    this.pulse(key, at);
    const span = this.active.get(key);
    if (!span) return undefined;
    span.event.endTime = at;
    span.event.status = status;
    this.active.delete(key);
    return span;
  }

  private addOnce(set: Set<string>, id: string, maximum: number): boolean {
    if (set.has(id)) return false;
    set.add(id);
    while (set.size > maximum) set.delete(set.values().next().value as string);
    return true;
  }

  /** Transactional batch copy; validation errors cannot accept half a batch. */
  clone(): PetEngineTelemetry {
    const next = new PetEngineTelemetry();
    const copy = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
    next.events = copy(this.events);
    const spans = new Map(next.events.map(event => [event.id, event]));
    next.active = new Map(Array.from(this.active, ([key, span]) => [key, {
      ...span,
      event: spans.get(span.event.id) ?? copy(span.event),
    }]));
    next.waiting = this.waiting ? spans.get(this.waiting.id) ?? copy(this.waiting) : undefined;
    next.sequence = this.sequence;
    next.lastTime = this.lastTime;
    next.lastObservedAt = this.lastObservedAt;
    next.turnId = this.turnId;
    next.turnOutcome = this.turnOutcome;
    next.terminalAt = this.terminalAt;
    next.lastFailedTool = this.lastFailedTool ? { ...this.lastFailedTool } : undefined;
    next.completedSpans = new Set(this.completedSpans);
    next.completedTurns = new Set(this.completedTurns);
    return next;
  }

  /** Mutates in place: clock, freshness and the event window change before
   * type-specific fields are checked. Callers that continue after a throw must
   * observe on a clone() and swap it in on success, as PetNative does. */
  observe(value: unknown, at: number): void {
    if (!Number.isFinite(at) || at < this.lastTime || at > PET_MAX_SECONDS * 1000)
      throw new Error('Invalid Engine pet clock.');
    if (!value || typeof value !== 'object' || Array.isArray(value))
      throw new Error('Invalid Engine pet metadata.');
    const event = value as Record<string, unknown>;
    const allowed = ['event', 'index', 'channel', 'span_id', 'activity_kind', 'action_id', 'outcome', 'id', 'worker_status', 'turn_id', 'turn_outcome'];
    const stringFields = ['action_id', 'event', 'span_id', 'id', 'worker_status', 'turn_id', 'turn_outcome'];
    if (Object.keys(event).some(key => !allowed.includes(key)) || typeof event.event !== 'string'
      || Object.values(event).some(v => typeof v === 'string' && v.length > 256)
      || stringFields.some(key => event[key] !== undefined && typeof event[key] !== 'string')
      || event.action_id !== undefined && (typeof event.action_id !== 'string' || !event.action_id
        || /[\u0000-\u001f\u007f-\u009f]/.test(event.action_id)
        || [...event.action_id].reduce((bytes, char) => {
          const cp = char.codePointAt(0)!;
          return bytes + (cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4);
        }, 0) > 256)
      || event.channel !== undefined && !['text', 'reasoning'].includes(event.channel as string)
      || event.activity_kind !== undefined && !ACTIVITY_KINDS.includes(event.activity_kind as OwnerActivityKind)
      || event.outcome !== undefined && (typeof event.outcome !== 'string'
        || (event.event === 'approval_resolved'
          ? !APPROVAL_OUTCOMES.includes(event.outcome as typeof APPROVAL_OUTCOMES[number])
          : event.event === 'operation_activity_completed'
            ? !OPERATION_OUTCOMES.includes(event.outcome as OwnerOperationOutcome)
            : true))
      || event.turn_outcome !== undefined && !TURN_OUTCOMES.includes(event.turn_outcome as OwnerTurnOutcome)
      || event.index !== undefined && (!Number.isSafeInteger(event.index) || (event.index as number) < 0))
      throw new Error('Invalid Engine pet metadata fields.');

    this.lastTime = at;
    this.lastObservedAt = at;
    this.events = this.events.filter(item => item.endTime >= at - 12_800);
    const required = (key: string): string => {
      const value = event[key];
      if (typeof value !== 'string' || !value) throw new Error(`Missing Engine ${key}.`);
      return value;
    };
    const index = (): string => {
      if (!Number.isSafeInteger(event.index)) throw new Error('Missing Engine index.');
      return String(event.index);
    };
    const activityKind = (): OwnerActivityKind => {
      if (!ACTIVITY_KINDS.includes(event.activity_kind as OwnerActivityKind))
        throw new Error('Missing Engine activity kind.');
      return event.activity_kind as OwnerActivityKind;
    };
    const startMessage = (key: string, kind: OwnerActivityKind, group: SpanGroup): void => {
      this.start(key, kind, group, at);
      this.waiting = undefined;
    };

    switch (event.event) {
      case 'turn_started': {
        const id = required('turn_id');
        if (this.turnId !== id) {
          this.active.clear();
          this.waiting = undefined;
          this.lastFailedTool = undefined;
          this.turnId = id;
          this.turnOutcome = undefined;
          this.terminalAt = undefined;
        }
        break;
      }
      case 'message_started': startMessage(`message:${index()}`, 'responding', 'responding'); break;
      case 'thinking_started': startMessage(`thinking:${index()}`, 'thinking', 'thinking'); break;
      case 'response_delta': {
        const reasoning = event.channel === 'reasoning';
        const key = `${reasoning ? 'thinking' : 'message'}:${index()}`;
        if (!this.active.has(key)) this.start(key, reasoning ? 'thinking' : 'responding', reasoning ? 'thinking' : 'responding', at);
        else this.pulse(key, at);
        this.waiting = undefined;
        break;
      }
      case 'message_complete': this.finish(`message:${index()}`, at); break;
      case 'thinking_complete': this.finish(`thinking:${index()}`, at); break;
      case 'operation_activity_started': {
        const spanId = required('span_id');
        const kind = activityKind();
        if (!this.completedSpans.has(spanId)) this.start(`operation:${spanId}`, kind, 'operation', at, 'parent', event.action_id as string | undefined);
        this.waiting = undefined;
        break;
      }
      case 'operation_activity_completed': {
        const spanId = required('span_id');
        const kind = activityKind();
        if (!OPERATION_OUTCOMES.includes(event.outcome as OwnerOperationOutcome))
          throw new Error('Missing Engine operation outcome.');
        const outcome = event.outcome as OwnerOperationOutcome;
        // A failure is recorded once, as its own onset event below; marking
        // the span `error` too would count it twice on the tape.
        const completed = this.finish(`operation:${spanId}`, at,
          outcome === 'succeeded' ? 'success' : 'unknown');
        if (completed && this.addOnce(this.completedSpans, spanId, 4096)) {
          if (outcome === 'failed') {
            this.add('operation_failed', 'error', at).status = 'error';
            this.lastFailedTool = { activityKind: kind, actionId: completed.actionId, failedAtMs: at };
          }
          if (outcome === 'denied' || outcome === 'cancelled') this.waiting = undefined;
        }
        break;
      }
      case 'tool_call_heartbeat':
        for (const [key, span] of this.active) if (span.group === 'operation') this.pulse(key, at);
        break;
      case 'approval_resolved': {
        required('id');
        if (!APPROVAL_OUTCOMES.includes(event.outcome as typeof APPROVAL_OUTCOMES[number]))
          throw new Error('Invalid Engine approval outcome.');
        this.waiting = undefined;
        break;
      }
      case 'agent_spawned': {
        const id = required('id');
        this.start(`agent:${id}`, 'delegating', 'agent', at, id);
        break;
      }
      case 'agent_progress': {
        const id = required('id');
        const key = `agent:${id}`;
        if (['completed', 'failed', 'cancelled', 'interrupted', 'budget_exhausted'].includes(event.worker_status as string)) {
          this.finish(key, at);
          break;
        }
        if (!this.active.has(key)) this.start(key, 'delegating', 'agent', at, id);
        else this.pulse(key, at);
        break;
      }
      case 'agent_complete': this.finish(`agent:${required('id')}`, at); break;
      case 'approval_required': case 'user_input_required': {
        required('id');
        if (!this.waiting) {
          this.waiting = this.add('human_request', 'human', at);
          this.waiting.status = 'pending';
        }
        break;
      }
      case 'turn_complete': {
        const outcome = event.turn_outcome;
        if (!TURN_OUTCOMES.includes(outcome as OwnerTurnOutcome))
          throw new Error('Missing Engine turn outcome.');
        const id = typeof event.turn_id === 'string' && event.turn_id.length ? event.turn_id : undefined;
        this.active.clear();
        this.waiting = undefined;
        this.turnId = id;
        // An outcome belongs to a turn. `/purge`, an edit rejection or a
        // session switch mid-turn completes with no turn id; recording the
        // outcome alone would break the projection invariant the Rust
        // contract checks (`turn_outcome` requires `turn_id`).
        this.turnOutcome = id ? outcome as OwnerTurnOutcome : undefined;
        this.terminalAt = at;
        if (outcome === 'completed' && id && this.addOnce(this.completedTurns, id, 256))
          this.add('turn_completed', 'communication', at).status = 'success';
        break;
      }
      default: throw new Error('Unsupported Engine pet event.');
    }
  }

  /** The typed shell may extend a request already witnessed in Engine events,
   * but a mid-turn attach cannot invent a NeedsYou state. */
  confirmWaiting(at: number, waiting: boolean): void {
    if (!waiting) {
      this.waiting = undefined;
      return;
    }
    if (this.waiting && at >= this.waiting.endTime) {
      this.waiting.endTime = at;
      this.lastObservedAt = at;
      this.lastTime = Math.max(this.lastTime, at);
    }
  }

  bucket(sequence: number): PetBucket {
    const end = (sequence + 1) * PET_BIN_MS;
    const input = this.events.filter(event => event.startTime < end && event.endTime >= end - 12_400)
      .map(event => ({ ...event, endTime: Math.min(event.endTime, end) }));
    return compilePetTelemetry(input, end, sequence)[0];
  }
}
