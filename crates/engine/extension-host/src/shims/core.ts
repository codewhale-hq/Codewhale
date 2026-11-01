/**
 * `exec.core`: a tool asking the Rust core to run one of the core's tools for it.
 *
 * Present on a tool's `exec` only while the call runs under the turn loop's
 * permission gate: the model called the extension tool directly and the core
 * gave that one call an invocation ticket (`tool/call.ticket`). A command, a
 * timer, activation code, a sub-agent's call, and a tool run from inside
 * `execute_tools` have no ticket, so they have no `exec.core`; the core would
 * refuse the request anyway, and this is only the early, local answer.
 *
 * The host never decides anything here. `core/call` goes through `plan_tool_calls`
 * and the approval gate exactly like a call the model makes, the approval card
 * is composed by Rust, and the answer is either the tool's result or a typed
 * refusal. The ticket is an opaque id the core minted for this invocation; it
 * is passed back unchanged and never shown to the plugin by any API here.
 */
import { ErrorCode, type Json, type OwnerRef, type ToolResultWire } from '../protocol.ts'
import { isJson } from '../json.ts'
import { RpcError, type RpcPeer } from '../rpc.ts'

/** Why the core did not run a call. */
export type CoreCallFailure =
  /** Policy: the core refuses this call outright, or the call's limits are used up. */
  | 'refused'
  /** The user declined the approval card. Do not retry. */
  | 'denied'
  /** The call was cancelled (the tool's own signal, a revocation, the turn). */
  | 'cancelled'
  /** The core could not serve it (the host is going away, the turn ended). */
  | 'unavailable'
  /** Anything else, including an input the host refused to send. */
  | 'failed'

export class CoreCallError extends Error {
  constructor(
    readonly code: CoreCallFailure,
    message: string,
  ) {
    super(message)
    this.name = 'CoreCallError'
  }
}

/** What a core tool returned. `isError` is the tool's own failure, not a refusal. */
export interface CoreCallResult {
  content: string
  isError: boolean
  structured?: Json
}

export interface CoreApi {
  /**
   * Run the core tool `name` with `input` and wait for its result. Rejects with
   * a `CoreCallError` when the core did not run it. `options.signal` cancels
   * this call; the tool's own `exec.signal` always does.
   */
  call(name: string, input?: Json, options?: { signal?: AbortSignal }): Promise<CoreCallResult>
}

const MAX_NAME_LENGTH = 128

function failureFor(error: unknown): CoreCallError {
  if (error instanceof CoreCallError) return error
  if (error instanceof RpcError) {
    switch (error.code) {
      case ErrorCode.Refused:
        return new CoreCallError('refused', error.message)
      case ErrorCode.Denied:
        return new CoreCallError('denied', error.message)
      case ErrorCode.Cancelled:
        return new CoreCallError('cancelled', error.message)
      case ErrorCode.NotAvailable:
        return new CoreCallError('unavailable', error.message)
      default:
        return new CoreCallError('failed', error.message)
    }
  }
  return new CoreCallError('failed', error instanceof Error ? error.message : String(error))
}

function toResult(wire: ToolResultWire): CoreCallResult {
  const result: CoreCallResult = {
    content: wire.content.map((block) => block.text).join('\n'),
    isError: wire.is_error,
  }
  if (wire.structured !== undefined) result.structured = wire.structured
  return result
}

/** The `exec.core` of one `tool/call` that carries `ticket`. Frozen. */
export function makeCoreApi(rpc: RpcPeer, owner: OwnerRef, ticket: string, callSignal: AbortSignal): Readonly<CoreApi> {
  return Object.freeze({
    async call(name: string, input: Json = {}, options: { signal?: AbortSignal } = {}): Promise<CoreCallResult> {
      if (typeof name !== 'string' || name.length === 0 || name.length > MAX_NAME_LENGTH) {
        throw new CoreCallError('failed', `core.call needs a tool name of 1 to ${MAX_NAME_LENGTH} characters`)
      }
      if (!isJson(input)) throw new CoreCallError('failed', 'core.call input must be plain JSON')
      const signal = options.signal === undefined ? callSignal : AbortSignal.any([callSignal, options.signal])
      try {
        return toResult(await rpc.request<ToolResultWire>('core/call', { owner, ticket, name, input }, signal))
      } catch (error) {
        throw failureFor(error)
      }
    },
  })
}
