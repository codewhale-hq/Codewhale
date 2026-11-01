/** Pinned Tier-0 orchestration. Rust holds commands, environment, approval and process trees. */
import type { OwnerRef, HarnessRunParams, Json } from '../protocol.ts'
import { matchesMatcher } from '../dsh/upstream/hooks/hook-protocol/src/matcher.ts'
import { parseHookOutput } from '../dsh/upstream/hooks/hook-protocol/src/codec.ts'
import { mergeHookOutputs } from '../dsh/upstream/hooks/hook-protocol/src/merge.ts'
import { isJson } from '../json.ts'
import { transformStockSnapshot } from './shared/stock-adapters.ts'

interface BrokerRpc { request<T = unknown>(method: string, params: unknown, signal?: AbortSignal): Promise<T> }
interface Output { success: boolean; stdout: string; stderr: string }
interface Result { hook_completed?:boolean;hook_skipped?:boolean;proposal?:string;ok: boolean; result?: { content: string; success: boolean; metadata?: Json }; error?: string }
const MAX_PENDING = 32
const MAX_STDOUT = 1024 * 1024
const MAX_STDERR = 64 * 1024

/** Legacy script JSON/plain-text semantics; no approval or launch facts cross this seam. */
export function normalizeOutput(value: unknown): Result {
  const out = value as Partial<Output> | null
  if (!out || typeof out.success !== 'boolean' || typeof out.stdout !== 'string' || typeof out.stderr !== 'string' || Buffer.byteLength(out.stdout) > MAX_STDOUT || Buffer.byteLength(out.stderr) > MAX_STDERR) throw new Error('invalid or oversized execution output')
  if (!out.success) return { ok: false, error: out.stderr ? (out.stdout ? `${out.stdout}\n${out.stderr}` : out.stderr) : out.stdout }
  try {
    const result = JSON.parse(out.stdout) as Record<string, unknown> | null
    if (result && typeof result.content === 'string' && typeof result.success === 'boolean' && (result.metadata === undefined || isJson(result.metadata))) {
      return { ok: true, result: { content: result.content, success: result.success, ...(result.metadata === undefined ? {} : { metadata: result.metadata as Json }) } }
    }
  } catch { /* Non-JSON stdout is a successful plain-text result, as in Rust's legacy caller. */ }
  return { ok: true, result: { content: out.stdout, success: true } }
}

export function createHarnessModule(rpc: BrokerRpc, reference: OwnerRef) {
  if (reference.plugin_id !== 'host:harness') throw new Error('execution runner requires its builtin owner')
  const owner = Object.freeze(structuredClone(reference))
  const stop = new AbortController()
  const pending = new Map<string, { abort: AbortController; done: Promise<Result> }>()
  let disposed = false
  return {
    async run(params: HarnessRunParams, signal: AbortSignal): Promise<Result> {
      if (disposed || signal.aborted || params.owner.plugin_id !== owner.plugin_id || params.owner.generation !== owner.generation || params.owner.owner_token !== owner.owner_token) throw new Error('execution runner owner is stale or cancelled')
      if (!params.execution_id || !params.ticket || !Number.isSafeInteger(params.deadline_ms) || params.deadline_ms < 1 || params.deadline_ms > (params.hook ? 2147483647 : 120_000) || pending.size >= MAX_PENDING || pending.has(params.execution_id)) throw new Error('execution is not admitted')
      if(params.hook) {
        const {event,dialect,point,matcher,query}=params.hook
        if(typeof event!=='string' || typeof point!=='string' || typeof query!=='string' || query.length>1024 || (matcher!==undefined && (typeof matcher!=='string' || matcher.length>1024)) || !['codewhale','claude-code','codex'].includes(dialect))throw new Error('invalid hook projection')
        if(dialect!=='codewhale' && !matchesMatcher(matcher,query,dialect as 'claude-code'|'codex'))return {ok:true,hook_skipped:true}
      }
      const abort = new AbortController()
      const cancel = () => abort.abort()
      signal.addEventListener('abort', cancel, { once: true }); stop.signal.addEventListener('abort', cancel, { once: true })
      const timer = setTimeout(cancel, params.deadline_ms)
      const done = (async () => {
        const cancelled = new Promise<never>((_, reject) => { abort.signal.addEventListener('abort', () => reject(new Error('execution cancelled')), { once: true }) })
        let value = await Promise.race([rpc.request('exec/redeem', { owner, execution_id: params.execution_id, ticket: params.ticket }, abort.signal), cancelled])
        if (disposed || abort.signal.aborted) throw new Error('execution cancelled')
        if(params.hook) {
          const out=value as any
          if(!out || out.kind!=='hook' || out.event!==params.hook.event || typeof out.success!=='boolean' || (out.exit_code!==null && !Number.isInteger(out.exit_code)))throw new Error('invalid hook completion projection')
          if(params.hook.event==='shell_env') {
            if('stdout' in out || 'stderr' in out || Object.keys(out).some(key=>!['kind','event','success','exit_code','keys'].includes(key)))throw new Error('ShellEnv projection contains private process data')
            return {ok:true,hook_completed:true}
          }
          if(params.hook.dialect==='codewhale')return {ok:true,hook_completed:true}
          if(typeof out.stdout!=='string' || typeof out.stderr!=='string' || Buffer.byteLength(out.stdout)>65536 || Buffer.byteLength(out.stderr)>65536)throw new Error('invalid dialect hook output')
          const decoded=parseHookOutput(out.exit_code===null?undefined:out.exit_code,out.stdout,out.stderr,params.hook.point)
          const merged=mergeHookOutputs([decoded])
          if(params.hook.event==='tool_call_before')return {ok:true,hook_completed:true,proposal:JSON.stringify({...(merged.decision==='deny'?{decision:'deny',reason:merged.reason ?? 'Blocked by hook'}:merged.decision==='ask' && params.hook.dialect!=='codex'?{decision:'ask',reason:merged.reason}:{}),...(merged.additionalContext.length?{additional_context:merged.additionalContext.join('\n\n')}:{})})}
          if(params.hook.event==='message_submit' && (merged.stop || decoded.updatedInput!==undefined || merged.additionalContext.length))throw new Error('hook requested unsupported prompt steering')
          if(params.hook.event==='message_submit')return {ok:true,hook_completed:true,proposal:JSON.stringify(merged.decision==='deny'?{block:true,reason:merged.reason ?? 'Blocked by hook'}:{})}
          if(merged.stop || merged.decision==='deny' || decoded.updatedInput!==undefined || merged.additionalContext.length)throw new Error('hook requested steering unavailable at this observer boundary')
          return {ok:true,hook_completed:true}
        }
        if (value && typeof value === 'object' && (value as Record<string, unknown>).kind === 'ocr_process') {
          if ((value as Record<string, unknown>).state !== 'native') throw new Error('OCR must begin with its admitted Native step')
          const first = transformStockSnapshot(value)
          if ((first.result.metadata as Record<string, unknown>).code !== 'fallback') return first
          const next = (value as Record<string, unknown>).next_ticket as string
          // A separate, single-use grant exists only after Core captured Native
          // failure/unavailability. No command/path/stage choice crosses IPC.
          value = await Promise.race([rpc.request('exec/redeem', { owner, execution_id: params.execution_id, ticket: next }, abort.signal), cancelled])
          if (disposed || abort.signal.aborted) throw new Error('execution cancelled')
          if (!value || typeof value !== 'object' || (value as Record<string, unknown>).kind !== 'ocr_process' || (value as Record<string, unknown>).state !== 'tesseract') throw new Error('OCR continuation returned the wrong stage')
          return transformStockSnapshot(value)
        }
        if (value && typeof value === 'object' && ['stock_adapter', 'pdf_process'].includes(String((value as Record<string, unknown>).kind))) return transformStockSnapshot(value)
        return normalizeOutput(value)
      })()
      pending.set(params.execution_id, { abort, done })
      try { return await done } finally { clearTimeout(timer); signal.removeEventListener('abort', cancel); stop.signal.removeEventListener('abort', cancel); pending.delete(params.execution_id) }
    },
    async dispose(): Promise<void> { if (disposed) return; disposed = true; stop.abort(); await Promise.allSettled([...pending.values()].map(row => row.done)) },
  }
}
