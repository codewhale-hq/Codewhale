import { CATEGORIES, clamp, errorOnsetOf, type Category, type WhaleEvent } from './model.js';
import { IntervalIndex } from './signal.js';
import { PET_MAX_SECONDS, validatePetState, type PetState } from './pet-sim.js';

/** One projection for imports, demos and recorded live snapshots. Times are ms.
 * These are aesthetic encodings of measured events, never model confidence. */
export const PET_BIN_MS = 400;
export interface PetBucket extends PetState {
  version: 1;
  sequence: number;
  simTimeMs: number;
  durationMs: number;
  onsets: number[];
  activeMs: number[];
  errors: number;
  agentIds: string[];
  waiting: boolean;
}

export function validatePetBucket(value: unknown): asserts value is PetBucket {
  validatePetState(value);
  const b = value as PetBucket;
  if (!b || typeof b !== 'object' || b.version !== 1 || !Number.isSafeInteger(b.sequence) || b.sequence < 0 || b.sequence > PET_MAX_SECONDS * 2.5
    || b.simTimeMs !== b.sequence * PET_BIN_MS || b.durationMs !== PET_BIN_MS
    || !CATEGORIES.includes(b.channel as Category) || typeof b.waiting !== 'boolean'
    || !Array.isArray(b.agentIds) || b.agentIds.length > 250_000 || b.agentIds.some(id => typeof id !== 'string' || !id || id.length > 4096)
    || !Number.isSafeInteger(b.errors) || b.errors < 0 || b.errors > 250_000
    || !Array.isArray(b.onsets) || b.onsets.length !== 13 || b.onsets.some(n => !Number.isSafeInteger(n) || n < 0 || n > 250_000)
    || !Array.isArray(b.activeMs) || b.activeMs.length !== 13 || b.activeMs.some(n => !Number.isFinite(n) || n < 0 || n > 100_000_000))
    throw new Error('Invalid version 1 pet bucket.');
}

export function decodePetJSONL(text: string): PetBucket[] {
  if (text.length > 64 * 1024 * 1024) throw new Error('Pet tape exceeds 64 MiB.');
  const rows = text.split(/\r?\n/).filter(line => line.trim()).map(line => JSON.parse(line) as unknown);
  if (rows.length > 216_000) throw new Error('Pet tape exceeds 24 hours.');
  return rows.map((row, i) => { validatePetBucket(row); if (row.sequence !== i) throw new Error('Non-contiguous pet tape.'); return row; });
}

/** A live file must advance before its contents count as a new observation.
 * Existing bytes, duplicate samples and a restarted sequence establish a
 * baseline; they never replay an old onset or human request. Drivers supply a
 * bounded tail and reset this cursor after suspension or a new attachment. */
export class PetLiveTape {
  private sequence: number | undefined;
  reset(): void { this.sequence = undefined; }
  readTail(text: string): PetBucket | undefined {
    if (!text) { this.reset(); return; }
    if (text.length > 262_144) { this.reset(); throw new Error('Live pet input exceeds its tail limit.'); }
    if (!text.endsWith('\n')) return;
    const line = text.trimEnd().split('\n').at(-1);
    if (!line) return;
    let packet: unknown;
    try { packet = JSON.parse(line); validatePetBucket(packet); }
    catch (error) { this.reset(); throw error; }
    const previous = this.sequence;
    if (previous === undefined) { this.sequence = packet.sequence; return; }
    if (packet.sequence === 0 && previous > 0) { this.sequence = 0; return; }
    if (packet.sequence <= previous) return;
    this.sequence = packet.sequence;
    return packet;
  }
}

const order = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;
const keyOf = (e: WhaleEvent) => JSON.stringify([e.traceId, e.id]);
const isContainer = (e: WhaleEvent) => e.attributes['whalesong.container'] === true
  || e.attributes['codewhale.container'] === true;

/** Compile a single trace. Unknown-duration spans provide onsets, not occupancy.
 * Updates of the same trace/id replace earlier snapshots rather than double count.
 * An endpoint onset gets its own bucket; intervals use [start, end). */
export function compilePetTelemetry(input: readonly WhaleEvent[], durationMs = 0, firstSequence = 0, originMs = 0): PetBucket[] {
  if (input.length > 250_000) throw new Error('Pet input exceeds 250000 events.');
  if (!Number.isFinite(durationMs) || durationMs < 0) throw new Error('Invalid pet duration.');
  if (!Number.isSafeInteger(firstSequence) || firstSequence < 0) throw new Error('Invalid first pet bucket.');
  if (!Number.isFinite(originMs)) throw new Error('Invalid pet clock origin.');
  durationMs = Math.max(0, durationMs - originMs);
  const unique = new Map<string, WhaleEvent>();
  const traces = new Set<string>();
  for (const e of input) {
    if (e.schemaVersion !== 1 || !e.id || !e.traceId || !CATEGORIES.includes(e.category)
      || !Number.isFinite(e.startTime) || !Number.isFinite(e.endTime)
      || e.startTime < 0 || e.endTime < e.startTime || !e.attributes)
      throw new Error('Invalid event-v1 pet input. Import through importTrace first.');
    traces.add(e.traceId); unique.set(keyOf(e), e);
  }
  if (traces.size > 1) throw new Error('Select one trace for the pet.');
  const parents = new Set([...unique.values()].filter(e => e.parentId).map(e => e.parentId!));
  const events = [...unique.values()].filter(e => !isContainer(e)
    && !(e.category === 'orchestration' && parents.has(e.id)))
    .map(e => ({ ...e, startTime: e.startTime - originMs, endTime: (e.openEnded ? e.startTime : e.endTime) - originMs,
      attributes: e.attributes['whalesong.error_onset_ms'] === undefined ? e.attributes
        : { ...e.attributes, 'whalesong.error_onset_ms': errorOnsetOf(e) - originMs } }))
    .sort((a, b) => a.startTime - b.startTime || order(a.id, b.id));
  let lastOnset = 0;
  for (const e of events) { durationMs = Math.max(durationMs, e.endTime); lastOnset = Math.max(lastOnset, e.startTime); }
  const failures = events.filter(e => e.category === 'error' || e.status === 'error').map(errorOnsetOf).sort((a, b) => a - b);
  if (failures.length) lastOnset = Math.max(lastOnset, failures[failures.length - 1]);
  const count = Math.max(1, Math.ceil(durationMs / PET_BIN_MS), Math.floor(lastOnset / PET_BIN_MS) + 1);
  if (count - firstSequence > 216_000 || count > PET_MAX_SECONDS * 2.5) throw new Error('Pet replay exceeds 24 hours; select a shorter trace.');
  const index = new IntervalIndex(events), result: PetBucket[] = [];
  const recent: WhaleEvent[] = [], names = new Map<string, number>();
  let next = 0, expired = 0, nextFailure = 0;
  for (let sequence = firstSequence; sequence < count; sequence++) {
    const start = sequence * PET_BIN_MS, end = start + PET_BIN_MS;
    // A trailing window only: appending future events cannot rewrite earlier bins.
    while (next < events.length && events[next].startTime < end) {
      const e = events[next++];
      // A liveness pulse continues an operation; it is not a repeated tool call.
      if (e.attributes['whalesong.continuation'] === true) continue;
      recent.push(e); names.set(e.name, (names.get(e.name) ?? 0) + 1);
    }
    while (expired < recent.length && recent[expired].startTime < end - 12_000) {
      const name = recent[expired++].name, n = names.get(name)! - 1;
      if (n) names.set(name, n); else names.delete(name);
    }
    const onsets = CATEGORIES.map(() => 0), activeMs = CATEGORIES.map(() => 0);
    const agents = new Set<string>();
    while (nextFailure < failures.length && failures[nextFailure] < start) nextFailure++;
    let errors = 0, waiting = false, human = false;
    while (nextFailure < failures.length && failures[nextFailure] < end) { errors++; nextFailure++; }
    for (const e of index.query(start, end)) {
      if (e.startTime >= end) continue;
      const onset = e.startTime >= start, c = CATEGORIES.indexOf(e.category);
      const overlap = Math.max(0, Math.min(end, e.endTime) - Math.max(start, e.startTime));
      if (!onset && !overlap) continue;
      activeMs[c] += overlap;
      if (onset) onsets[c]++;
      if (e.agentId && !['unknown', 'unattributed'].includes(e.agentId)) agents.add(e.agentId);
      if (e.category === 'human') {
        human = true; waiting ||= e.status === 'pending' || e.status === 'running' || e.attributes['whalesong.waiting'] === true;
      }
    }
    const total = activeMs.reduce((a, b) => a + b, 0), hits = onsets.reduce((a, b) => a + b, 0);
    const observed = total > 0 || hits > 0 || errors > 0;
    let dominant = CATEGORIES.indexOf('other');
    for (let c = 0; c < CATEGORIES.length; c++) {
      if (activeMs[c] > activeMs[dominant]
        || activeMs[c] === activeMs[dominant] && onsets[c] > onsets[dominant]) dominant = c;
    }
    let repeated = 0;
    for (const n of names.values()) if (n >= 4) repeated += n;
    const repeatDensity = repeated / Math.max(1, recent.length - expired);
    const channel: Category = errors ? 'error' : human ? 'human' : agents.size >= 3 ? 'agent' : CATEGORIES[dominant];
    result.push({ version: 1, sequence, simTimeMs: start, durationMs: PET_BIN_MS,
      activity: observed ? clamp(.28 + .38 * total / PET_BIN_MS + .08 * hits, 0, 1) : .12,
      coherence: observed ? clamp(.92 - repeatDensity * .58 - Math.min(.55, errors * .18), .08, 1) : .25,
      attention: waiting ? .8 : human ? .65 : errors ? .45 : 0,
      channel, observed: observed ? 1 : 0, roamX: 0, roamY: 0, flip: 1, lit: 1,
      onsets, activeMs, errors, agentIds: [...agents].sort(order), waiting });
  }
  return result;
}

/** Flat state remains compatible with native readers; metadata preserves audio
 * onsets and peer identities. No prompts, tool arguments, or event names escape. */
export function encodePetJSONL(buckets: readonly PetBucket[]): string {
  return buckets.map(b => JSON.stringify(b)).join('\n') + (buckets.length ? '\n' : '');
}

/** Legacy visual conformance interchange. Use JSONL to preserve audio metadata. */
export function encodePetTSV(buckets: readonly PetBucket[]): string {
  return 'dt\tactivity\tcoherence\tattention\tchannel\tobserved\troamX\troamY\tflip\tlit\n'
    + buckets.map(b => [b.durationMs / 1000, b.activity, b.coherence, b.attention, b.channel,
      b.observed, b.roamX, b.roamY, b.flip, b.lit].join('\t')).join('\n') + '\n';
}
