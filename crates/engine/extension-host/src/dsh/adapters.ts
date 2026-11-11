import type { Service } from '@deepseek-ai/cordis'
import type { DshService } from '../protocol.generated.ts'
import type { OwnerRecord, ToolInvocation } from '../root.ts'
import type { RpcPeer } from '../rpc.ts'
import type { HostTier } from '../tier.ts'
import { sessionsAdapter, systemPromptAdapter } from './session-prompt.ts'
import { DSH_AGENT_ADAPTERS } from './agents.ts'
import { remoteAdapter } from './remote.ts'

export interface AdapterHost {
  readonly tier: HostTier
  readonly rpc: RpcPeer
  ownerOf(ctx: unknown): OwnerRecord | undefined
  liveInvocation(ctx: unknown): ToolInvocation | undefined
  broker(ctx: unknown, service: DshService, method: string, input: unknown): Promise<unknown>
  log(level: 'info' | 'warn' | 'error', message: string, owner?: OwnerRecord): void
}

export interface DshAdapter {
  readonly service: string
  define(host: AdapterHost): new (ctx: any) => Service
}

export const DSH_ADAPTERS: readonly DshAdapter[] = [sessionsAdapter, systemPromptAdapter, ...DSH_AGENT_ADAPTERS, remoteAdapter]
