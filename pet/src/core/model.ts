/** Versioned, vendor-neutral trace and signal contracts. All internal time is ms. */
export const CATEGORIES = [
  'reasoning', 'tool', 'memory', 'code', 'filesystem', 'network', 'browser',
  'communication', 'agent', 'orchestration', 'error', 'human', 'other',
] as const;
export type Category = typeof CATEGORIES[number];
export type Status = 'pending' | 'running' | 'success' | 'error' | 'unknown';
export type PrivacyMode = 'redact' | 'metadata' | 'retain';
export interface WhaleEvent {
  schemaVersion: 1;
  id: string; traceId: string; parentId?: string;
  startTime: number; endTime: number; openEnded?: boolean;
  agentId: string; agentType?: string; parentAgentId?: string;
  category: Category; subtype?: string; name: string;
  model?: string; provider?: string; tool?: string;
  inputTokens?: number; outputTokens?: number; cachedTokens?: number;
  cost?: number; costCurrency?: string; latency?: number;
  contextTokens?: number; contextLimit?: number; retry?: number;
  status: Status;
  sourceId?: string; targetId?: string; targetType?: string;
  links?: { traceId: string; spanId: string; attributes?: Record<string, unknown> }[];
  attributes: Record<string, unknown>; payload?: unknown; raw?: unknown;
}
export interface Trace {
  id: string; name: string; description?: string; events: WhaleEvent[];
  duration: number; originTime?: string; source: 'demo' | 'jsonl' | 'otlp' | 'codewhale' | 'platform';
  privacy: PrivacyMode; warnings: string[]; metadata: Record<string, unknown>;
}
export interface BinLevel {
  binMs: number; length: number;
  /** Channel-major arrays, index = channel * length + time bin. */
  onsets: Float64Array; activeMs: Float64Array; outputTokens: Float64Array;
  cost: Float64Array; errors: Float64Array; peak: Float64Array;
}
export interface SignalPyramid {
  duration: number; channels: readonly Category[]; levels: BinLevel[];
  /** Fixed calibration across zoom levels; sharing this locks A/B scales. */
  calibration: Record<Metric, number>;
}
export type Metric = 'activity' | 'onsets' | 'tokens' | 'cost';
export type FindingKind = 'loop' | 'burst' | 'gap' | 'retry' | 'divergence' | 'spawn' | 'context' | 'boundary';
export interface Finding {
  id: string; kind: FindingKind; severity: 'info' | 'warning' | 'critical';
  title: string; detail: string; startTime: number; endTime: number;
  agentId?: string; eventIds: string[]; evidence: Record<string, unknown>;
}
export interface Fingerprint {
  version: 1; channels: number[]; temporal: number[]; autocorrelation: number[];
  features: Record<string, number>; vector: number[];
}
export interface Statistics {
  events: number; agents: number; duration: number; errors: number; retries: number;
  inputTokens: number; outputTokens: number; cachedTokens: number;
  cost: number; costKnown: boolean; costCount: number; tokenCount: number; outputTokenCount: number; inputTokenCount: number;
  latencyP50: number; latencyP95: number; observedGapRatio: number;
}
export interface Analysis {
  sessionId?: string;
  trace: Trace; pyramid: SignalPyramid; findings: Finding[];
  fingerprint: Fingerprint; stats: Statistics; computeMs: number;
}
export interface Filters {
  query: string; category: string; agent: string; model: string; tool: string; status: string;
}
export const EMPTY_FILTERS: Filters = { query: '', category: '', agent: '', model: '', tool: '', status: '' };
export function eventMatches(e: WhaleEvent, f: Filters): boolean {
  if (f.category && e.category !== f.category) return false;
  if (f.agent && e.agentId !== f.agent) return false;
  if (f.model && e.model !== f.model) return false;
  if (f.tool && e.tool !== f.tool) return false;
  if (f.status && e.status !== f.status) return false;
  if (f.query) {
    const q = f.query.toLowerCase();
    // Search is deliberately content-aware but runs only over locally retained fields.
    if (![e.name, e.id, e.agentId, e.model, e.tool, e.provider, e.category,
      JSON.stringify(e.attributes), JSON.stringify(e.payload), JSON.stringify(e.raw)].filter(Boolean).join(' ').toLowerCase().includes(q)) return false;
  }
  return true;
}
export const durationOf = (e: WhaleEvent): number => Math.max(0, e.endTime - e.startTime);
/** An explicit failure receipt can arrive after a span began or ended. Keep
 * its timestamp distinct from the operation onset in every signal view. */
export function errorOnsetOf(e: WhaleEvent): number {
  const time = e.attributes['whalesong.error_onset_ms'];
  if (time === undefined) return e.startTime;
  if (typeof time !== 'number' || !Number.isFinite(time) || time < e.startTime)
    throw new Error('Invalid failure observation time.');
  return time;
}
export function stableHash(text: string, seed = 2166136261): number {
  let h = seed;
  for (let i = 0; i < text.length; i++) { h ^= text.charCodeAt(i); h = Math.imul(h, 16777619); }
  return h >>> 0;
}
export function quantile(a: number[], q: number): number {
  if (!a.length) return 0;
  const s = [...a].sort((a, b) => a - b), x = Math.min(1, Math.max(0, q)) * (s.length - 1);
  return s[Math.floor(x)] + (s[Math.ceil(x)] - s[Math.floor(x)]) * (x % 1);
}
export function clamp(x: number, lo: number, hi: number): number { return Math.max(lo, Math.min(hi, x)); }
export function formatTime(ms: number, precise = false): string {
  const m = Math.floor(Math.max(0, ms) / 60000), s = Math.floor(Math.max(0, ms) / 1000) % 60;
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}${precise ? '.' + String(Math.floor(ms % 1000)).padStart(3, '0') : ''}`;
}
