import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import type { DshAdapter } from './adapters.ts'

function label(value: unknown): string | undefined {
  if (value === undefined) return undefined
  if (typeof value !== 'string') throw new RpcError(ErrorCode.InvalidParams, 'remote.pair label must be a string')
  return value
}

function grant(value: unknown): string {
  if (typeof value !== 'string' || value.length === 0 || value.length > 128) throw new RpcError(ErrorCode.InvalidParams, 'remote.revoke needs the grant id remote.pair returned')
  return value
}

export const remoteAdapter: DshAdapter = {
  service: 'remote',
  define(host) {
    class RemoteService extends Service {
      constructor(ctx: any) { super(ctx, 'remote') }
      status() { return host.broker(this.ctx, 'remote', 'status', {}) }
      pair(options?: { label?: unknown }) {
        const value = label(options?.label)
        return host.broker(this.ctx, 'remote', 'pair', value === undefined ? {} : { label: value })
      }
      clients() { return host.broker(this.ctx, 'remote', 'clients', {}) }
      revoke(id: unknown) { return host.broker(this.ctx, 'remote', 'revoke', { grant: grant(id) }) }
    }
    Object.freeze(RemoteService.prototype)
    return RemoteService
  },
}
