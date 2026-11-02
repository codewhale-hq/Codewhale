/** A pre-execute mod proposes changes; Rust owns folding and admission. */
import { isJson } from '../json.ts'
import type { HookCallPayload, HookVerdictWire } from '../protocol.ts'
import type { OwnedEntry, OwnerBase } from './owned.ts'

export interface LocalHook<O extends OwnerBase> extends OwnedEntry<O> {
  callback: (exec: Readonly<HookExecution>, next: () => Promise<HookVerdictWire>) => unknown
}

/** DSH's call-view spelling, without its agent/runtime/token authorities. */
export interface HookExecution {
  readonly name: string
  readonly callId: string
  readonly arguments: unknown
  readonly signal: AbortSignal
  readonly workspace: string
  readonly mode: string
  readonly model: string
}

function freezeJson(value: any): any {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) freezeJson(child)
    Object.freeze(value)
  }
  return value
}

export function hookExecution(payload: HookCallPayload, signal: AbortSignal): Readonly<HookExecution> {
  return Object.freeze({
    name: payload.name,
    callId: payload.call_id,
    arguments: freezeJson(payload.input),
    signal,
    workspace: payload.workspace,
    mode: payload.mode,
    model: payload.model,
  })
}

/** Unsupported or malformed decisions throw, so the Rust strict hook fails closed. */
export function hookVerdict(value: unknown, onAllow: () => void): HookVerdictWire {
  if (value === undefined || value === null) return { kind: 'abstain' }
  if (!isJson(value) || typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new TypeError('pre-execute listener must return a JSON verdict')
  }
  const result = value as Record<string, any>
  switch (result.kind) {
    case 'allow':
      onAllow()
      return { kind: 'abstain' }
    case 'abstain':
      return { kind: 'abstain' }
    case 'deny':
      if (typeof result.reason !== 'string') throw new TypeError('deny needs a reason')
      return { kind: 'deny', reason: result.reason }
    case 'ask':
      if (result.reason !== undefined && typeof result.reason !== 'string') throw new TypeError('ask reason must be text')
      return { kind: 'ask', reason: result.reason ?? 'Requested by a pre-execute listener' }
    case 'annotate':
      if (typeof result.text !== 'string') throw new TypeError('annotate needs text')
      return { kind: 'annotate', text: result.text }
    case 'revise':
      if (!result.input || typeof result.input !== 'object' || Array.isArray(result.input)) {
        throw new TypeError('revise needs a JSON object input')
      }
      return { kind: 'revise', input: result.input }
    default:
      throw new TypeError(`unsupported pre-execute verdict ${JSON.stringify(result.kind)}`)
  }
}
