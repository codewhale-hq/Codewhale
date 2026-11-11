import type { Service } from '@deepseek-ai/cordis'
import type { OwnerRecord, ToolInvocation } from '../root.ts'
import type { RpcPeer } from '../rpc.ts'
import type { HostTier } from '../tier.ts'

export interface AdapterHost {
  readonly tier: HostTier
  readonly rpc: RpcPeer
  ownerOf(ctx: unknown): OwnerRecord | undefined
  liveInvocation(ctx: unknown): ToolInvocation | undefined
  log(level: 'info' | 'warn' | 'error', message: string, owner?: OwnerRecord): void
}

export interface DshAdapter {
  readonly service: string
  define(host: AdapterHost): new (ctx: any) => Service
}

export const DSH_ADAPTERS: readonly DshAdapter[] = []
