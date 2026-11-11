import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import type { OwnerRecord, ToolInvocation } from '../root.ts'
import type { AdapterHost, DshAdapter } from './adapters.ts'

const CHILD_ONLY = "system prompt sections are only available on a child agent's agent/created context"

function checkpointCall(host: AdapterHost, ctx: unknown, session: unknown): ToolInvocation | undefined {
  const invocation = host.liveInvocation(ctx)
  const ticket = invocation?.ticket
  return invocation !== undefined && typeof ticket === 'string' && ticket.length > 0
    && session !== undefined && session === invocation.session
    ? invocation
    : undefined
}

export const sessionsAdapter: DshAdapter = {
  service: 'sessions',
  define(host) {
    class SessionsService extends Service {
      constructor(ctx: any) { super(ctx, 'sessions') }

      async flush(session: unknown): Promise<boolean> {
        const ctx = this.ctx
        const call = checkpointCall(host, ctx, session)
        if (call === undefined) throw new RpcError(ErrorCode.NotAvailable, 'sessions.flush requires this live tool invocation’s session')
        const owner: OwnerRecord = call.owner
        const live = () => checkpointCall(host, ctx, session) === call && !call.signal.aborted
        const run = async () => {
          if (!live()) throw new RpcError(ErrorCode.NotAvailable, 'session checkpoint invocation ended')
          const result = await host.rpc.request<unknown>('session/checkpoint', { owner: owner.ref, ticket: call.ticket }, call.signal)
          if (!live()) throw new RpcError(ErrorCode.NotAvailable, 'session checkpoint invocation ended')
          if (typeof result !== 'boolean') throw new RpcError(ErrorCode.Internal, 'invalid session checkpoint receipt')
          return result
        }
        const result = call.pending.then(run)
        call.pending = result.then(() => undefined, () => undefined)
        return result
      }
    }
    Object.freeze(SessionsService.prototype)
    return SessionsService
  },
}

export const systemPromptAdapter: DshAdapter = {
  service: 'systemPrompt',
  define() {
    class SystemPromptService extends Service {
      constructor(ctx: any) { super(ctx, 'systemPrompt') }
      section() { throw new RpcError(ErrorCode.NotAvailable, CHILD_ONLY) }
      getSectionOrder() { throw new RpcError(ErrorCode.NotAvailable, CHILD_ONLY) }
    }
    Object.freeze(SystemPromptService.prototype)
    return SystemPromptService
  },
}
