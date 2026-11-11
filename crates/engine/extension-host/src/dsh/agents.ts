import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import { PERSONA_PREFIX_SECTION } from './dsh-message-compat.js'
import type { AdapterHost, DshAdapter } from './adapters.ts'

const SPAWN_PROVIDER = Object.freeze({
  name: 'spawn',
  inheritsParentContext: false,
  capabilities: Object.freeze({ persona: true, toolFilter: true, agentOptions: true }),
})
const PERSONA_ORDER = 0

function plain(value: unknown, allowed: string[], what: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)
    || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) throw new RpcError(ErrorCode.InvalidParams, `expected a plain ${what} object`)
  const result: Record<string, unknown> = Object.create(null)
  for (const key of Reflect.ownKeys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key)!
    if (typeof key !== 'string' || !allowed.includes(key) || !('value' in descriptor)) throw new RpcError(ErrorCode.NotAvailable, `unsupported ${what} property ${String(key)}`)
    if (descriptor.value !== undefined) result[key] = descriptor.value
  }
  return result
}

function defineSubagents(host: AdapterHost): new (ctx: any) => Service {
  class SubagentsShim extends Service {
    constructor(ctx: any) { super(ctx, 'subagents') }
    getProvider(name: unknown) { return name === 'spawn' ? SPAWN_PROVIDER : undefined }
    async startActivation(request: any) {
      const owner = host.ownerOf(this.ctx)
      const input = plain(request, ['provider', 'label', 'childId', 'request', 'signal', 'delivery'], 'activation')
      if (input.provider !== 'spawn') throw new RpcError(ErrorCode.NotAvailable, 'only the spawn provider is available')
      const spawn = plain(input.request, ['parent', 'prompt', 'agentOptions'], 'spawn request')
      const options = spawn.agentOptions === undefined ? {} : plain(spawn.agentOptions, ['model'], 'agentOptions')
      if (options.model !== undefined && typeof options.model !== 'string') throw new RpcError(ErrorCode.InvalidParams, 'agentOptions.model must be a string')
      const delivery = input.delivery === undefined ? 'parent' : input.delivery
      if (delivery !== 'caller' && delivery !== 'parent') throw new RpcError(ErrorCode.InvalidParams, 'delivery must be caller or parent')
      const childId = input.childId
      if (typeof childId !== 'string' || !/^[A-Za-z0-9_.:-]{1,128}$/.test(childId)) throw new RpcError(ErrorCode.InvalidParams, 'childId must be 1-128 letters, digits, or _.:-')
      if (!Array.isArray(spawn.prompt) || spawn.prompt.length === 0) throw new RpcError(ErrorCode.InvalidParams, 'prompt must be a non-empty content array')
      const prompt = spawn.prompt.map((block: unknown) => {
        const item = plain(block, ['type', 'text'], 'prompt block')
        if (item.type !== 'text' || typeof item.text !== 'string') throw new RpcError(ErrorCode.NotAvailable, 'only text prompt blocks are supported')
        return item.text
      }).join('')
      if (input.signal !== undefined && !(input.signal instanceof AbortSignal)) throw new RpcError(ErrorCode.InvalidParams, 'signal must be an AbortSignal')
      const invocation = host.liveInvocation(this.ctx)
      if (!invocation || invocation.agent === undefined || spawn.parent !== invocation.agent) {
        throw new RpcError(ErrorCode.NotAvailable, 'startActivation requires the exact live calling agent as parent')
      }
      if (invocation.owner !== owner) throw new RpcError(ErrorCode.NotAvailable, 'subagents service owner is no longer current')
      if (invocation.delegated || invocation.subagentTicket === undefined) throw new RpcError(ErrorCode.NotAvailable, 'child agents are only available to a root tool invocation')
      const signal = input.signal === undefined ? invocation.signal : AbortSignal.any([invocation.signal, input.signal as AbortSignal])
      signal.throwIfAborted()
      let persona: string | undefined
      const denied = new Set<string>()
      let open = true
      const sealed = () => { if (!open) throw new RpcError(ErrorCode.NotAvailable, 'the child creation window has closed') }
      const child = Object.freeze({
        id: childId,
        ctx: Object.freeze({
          systemPrompt: Object.freeze({
            getSectionOrder(name: unknown) {
              sealed()
              if (name !== 'DEPLOYMENT_PERSONA_PREFIX') throw new RpcError(ErrorCode.NotAvailable, 'only the deployment persona slot is available')
              return PERSONA_ORDER
            },
            section(section: unknown) {
              sealed()
              const value = plain(section, ['name', 'order', 'text'], 'system prompt section')
              if (value.name !== PERSONA_PREFIX_SECTION || value.order !== PERSONA_ORDER || typeof value.text !== 'string') {
                throw new RpcError(ErrorCode.NotAvailable, 'only the child deployment persona section is supported')
              }
              if (persona !== undefined) throw new RpcError(ErrorCode.InvalidParams, 'the child persona is already set')
              if (Buffer.byteLength(value.text) > 32 * 1024) throw new RpcError(ErrorCode.InvalidParams, 'child persona exceeds 32768 bytes')
              persona = value.text
              return () => undefined
            },
          }),
          tools: Object.freeze({
            restrict(filter: unknown) {
              sealed()
              const value = plain(filter, ['deny'], 'tool filter')
              if (!Array.isArray(value.deny) || value.deny.some(name => typeof name !== 'string' || name === '')) throw new RpcError(ErrorCode.InvalidParams, 'deny must be a list of tool names')
              for (const name of value.deny as string[]) denied.add(name)
              return () => undefined
            },
          }),
        }),
      })
      const listeners = (((this.ctx as any).root?.events?._hooks?.['agent/created']) ?? [])
        .filter((hook: any) => !hook.global && hook.ctx !== undefined && host.ownerOf(hook.ctx) === owner)
        .map((hook: any) => hook.callback)
      try {
        await Promise.all(listeners.map(async (callback: Function) => callback({ agent: child })))
      } finally {
        open = false
      }
      signal.throwIfAborted()
      const call = invocation
      const rpc = (method: string, params: Record<string, unknown>, abort: AbortSignal) =>
        host.rpc.request<any>(method, { owner: call.owner.ref, ticket: call.subagentTicket, child_id: childId, ...params }, abort)
      const started = await rpc('subagent/start', {
        prompt,
        ...(options.model === undefined ? {} : { model: options.model }),
        ...(persona === undefined ? {} : { persona }),
        denied_tools: [...denied],
        delivery,
      }, signal)
      if (started === null || typeof started !== 'object' || started.child_id !== childId || typeof started.message_id !== 'string') {
        throw new RpcError(ErrorCode.Internal, 'invalid child activation receipt')
      }
      let result: Promise<unknown> | undefined
      let disposed: Promise<void> | undefined
      return Object.freeze({
        childId,
        messageId: started.message_id,
        get result() {
          result ??= rpc('subagent/result', {}, call.signal).then((value: any) => {
            if (value === null || typeof value !== 'object' || typeof value.stop_reason !== 'string' || !Array.isArray(value.output)) {
              throw new RpcError(ErrorCode.Internal, 'invalid child result receipt')
            }
            return Object.freeze({
              stopReason: value.stop_reason,
              output: Object.freeze(value.output.map((block: any) => Object.freeze({ type: 'text', text: String(block.text) }))),
              ...(value.omitted_blocks > 0 ? { diagnostic: `${value.omitted_blocks} non-text output block(s) are not supported` } : {}),
            })
          })
          return result
        },
        dispose() {
          disposed ??= rpc('subagent/dispose', {}, call.signal).then(() => undefined)
          return disposed
        },
      })
    }
  }
  Object.freeze(SubagentsShim.prototype)
  return SubagentsShim
}

function defineAgents(): new (ctx: any) => Service {
  class AgentsShim extends Service {
    constructor(ctx: any) { super(ctx, 'agents') }
  }
  Object.freeze(AgentsShim.prototype)
  return AgentsShim
}

export const DSH_AGENT_ADAPTERS: readonly DshAdapter[] = Object.freeze([
  { service: 'subagents', define: defineSubagents },
  { service: 'agents', define: defineAgents },
])
