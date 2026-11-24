import type { Service } from '@deepseek-ai/cordis'
import type { DshService } from '../protocol.generated.ts'
import type { OwnerRecord, ToolInvocation } from '../root.ts'
import type { RpcPeer } from '../rpc.ts'
import type { HostTier } from '../tier.ts'
import { sessionsAdapter, systemPromptAdapter } from './session-prompt.ts'
import { DSH_AGENT_ADAPTERS } from './agents.ts'
import { remoteAdapter } from './remote.ts'
import { workspaceAdapter } from './workspace.ts'
import { llmAdapter } from './llm.ts'
import { credentialsAdapter } from './network.ts'
import { settingsAdapter } from './settings.ts'
import { pluginManagerAdapter, deepseekAccountAdapter } from './catalog.ts'
import { terminalAdapter } from './terminal.ts'
import { attachmentsAdapter } from './attachments.ts'
import { DSH_HOST_SERVICE_ADAPTERS } from './host-services.ts'
export { BUILTIN_DSH_ADAPTERS } from './plugin-manager.ts'

export interface AdapterHost {
  readonly tier: HostTier
  readonly rpc: RpcPeer
  ownerOf(ctx: unknown): OwnerRecord | undefined
  liveInvocation(ctx: unknown): ToolInvocation | undefined
  brokerTicket(ctx: unknown, service: DshService): { invocation: ToolInvocation, ticket: string }
  broker(ctx: unknown, service: DshService, method: string, input: unknown, signal?: AbortSignal): Promise<unknown>
  log(level: 'info' | 'warn' | 'error', message: string, owner?: OwnerRecord): void
}

export interface DshAdapter {
  readonly service: string
  define(host: AdapterHost): new (ctx: any) => Service
}

export const DSH_ADAPTERS: readonly DshAdapter[] = [
  sessionsAdapter,
  systemPromptAdapter,
  ...DSH_AGENT_ADAPTERS,
  remoteAdapter,
  workspaceAdapter,
  llmAdapter,
  credentialsAdapter,
  settingsAdapter,
  pluginManagerAdapter,
  deepseekAccountAdapter,
  terminalAdapter,
  attachmentsAdapter,
  ...DSH_HOST_SERVICE_ADAPTERS,
]
