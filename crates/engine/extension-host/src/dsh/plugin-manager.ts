import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import type { AdapterHost, DshAdapter } from './adapters.ts'

interface ChangeResult {
  changed: boolean
  application: 'applied' | 'cancelled' | 'overridden' | 'restart-required' | 'failed'
  stage?: string
  target?: string
  error?: { code: string, diagnostic: string, message: string }
  [key: string]: unknown
}

const EXEMPTIONS_DISABLED = 'Codewhale does not exempt plugins from the host compatibility contract.'
const APPLICATIONS = new Set(['applied', 'cancelled', 'overridden', 'restart-required', 'failed'])

function invalid(what: string): RpcError {
  return new RpcError(ErrorCode.Internal, `invalid plugins broker ${what}`)
}

function record(value: unknown, what: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw invalid(what)
  return value as Record<string, unknown>
}

function named(value: unknown, what: string): string {
  if (typeof value !== 'string' || value.length === 0) {
    throw new RpcError(ErrorCode.InvalidParams, `${what} must be a non-empty string`)
  }
  return value
}

function booleanFlag(value: unknown, what: string): boolean {
  if (typeof value !== 'boolean') {
    throw new RpcError(ErrorCode.InvalidParams, `${what} must be a boolean`)
  }
  return value
}

function changeResult(value: unknown, what: string): ChangeResult {
  const row = record(value, what)
  if (typeof row.changed !== 'boolean' || typeof row.application !== 'string' || !APPLICATIONS.has(row.application)) {
    throw invalid(what)
  }
  return row as ChangeResult
}

function requireCreator(host: AdapterHost, ctx: unknown, service: string): void {
  if (host.tier !== 'builtin' || host.ownerOf(ctx)?.ref.plugin_id !== 'host:creator') {
    throw new RpcError(ErrorCode.Refused, `${service} is only available to the built-in creator module`)
  }
  if (host.liveInvocation(ctx) === undefined) {
    throw new RpcError(ErrorCode.Refused, `${service} requires a live tool invocation`)
  }
}

export const builtinPluginManagerAdapter: DshAdapter = {
  service: 'pluginManager',
  define(host) {
    class BuiltinPluginManagerService extends Service {
      constructor(ctx: any) {
        super(ctx, 'pluginManager')
      }

      async listBundles(): Promise<unknown[]> {
        requireCreator(host, this.ctx, 'pluginManager')
        const answer = record(await host.broker(this.ctx, 'plugins', 'list', {}), 'list')
        if (!Array.isArray(answer.entries)) throw invalid('list')
        return answer.entries
      }

      async listPlugins(signal?: AbortSignal): Promise<unknown[]> {
        requireCreator(host, this.ctx, 'pluginManager')
        const answer = record(await host.broker(this.ctx, 'plugins', 'status', {}, signal), 'status')
        if (!Array.isArray(answer.entries)) throw invalid('status')
        return answer.entries
      }

      async listTools(signal?: AbortSignal): Promise<string[]> {
        requireCreator(host, this.ctx, 'pluginManager')
        const answer = record(await host.broker(this.ctx, 'plugins', 'tools', {}, signal), 'tools')
        const list = answer.names ?? answer.tools
        if (!Array.isArray(list)) throw invalid('tools')
        return list.map((item) => String(item))
      }

      async setPluginEnabled(target: unknown, enabled: unknown): Promise<ChangeResult> {
        requireCreator(host, this.ctx, 'pluginManager')
        const entry = named(target, 'target')
        const method = booleanFlag(enabled, 'enabled') ? 'enable' : 'disable'
        return changeResult(await host.broker(this.ctx, 'plugins', method, { target: entry, row: entry }), method)
      }

      async setBundleEnabled(target: unknown, enabled: unknown): Promise<ChangeResult> {
        requireCreator(host, this.ctx, 'pluginManager')
        const bundle = named(target, 'target')
        const method = booleanFlag(enabled, 'enabled') ? 'enable' : 'disable'
        return changeResult(await host.broker(this.ctx, 'plugins', method, { target: bundle }), method)
      }

      async installBundle(
        target: unknown,
        options?: { enabled?: unknown, approvedBuilds?: unknown, registry?: unknown } | null,
      ): Promise<ChangeResult> {
        requireCreator(host, this.ctx, 'pluginManager')
        const spec = named(target, 'target')
        if (options !== undefined && options !== null && (typeof options !== 'object' || Array.isArray(options))) {
          throw new RpcError(ErrorCode.InvalidParams, 'options must be an object')
        }
        const payload: Record<string, unknown> = { target: spec }
        if (options?.enabled !== undefined) payload.enabled = booleanFlag(options.enabled, 'enabled')
        if (options?.approvedBuilds !== undefined) {
          if (!Array.isArray(options.approvedBuilds) || options.approvedBuilds.some((name) => typeof name !== 'string' || name.length === 0)) {
            throw new RpcError(ErrorCode.InvalidParams, 'approvedBuilds must be an array of non-empty strings')
          }
          payload.approved_builds = [...options.approvedBuilds]
        }
        if (options?.registry !== undefined && options.registry !== null) {
          payload.registry = named(options.registry, 'registry')
        }
        return changeResult(await host.broker(this.ctx, 'plugins', 'install', payload), 'install')
      }

      async removeBundle(target: unknown): Promise<ChangeResult> {
        requireCreator(host, this.ctx, 'pluginManager')
        const bundle = named(target, 'target')
        return changeResult(await host.broker(this.ctx, 'plugins', 'remove', { target: bundle }), 'remove')
      }

      listVersionExemptions(): { exemptions: [], warnings: string[] } {
        requireCreator(host, this.ctx, 'pluginManager')
        return { exemptions: [], warnings: [EXEMPTIONS_DISABLED] }
      }

      async setVersionExemption(
        target: unknown,
        runtimeVersion: unknown,
        enabled: unknown,
        _acceptRisk?: unknown,
      ): Promise<ChangeResult> {
        requireCreator(host, this.ctx, 'pluginManager')
        named(target, 'target')
        named(runtimeVersion, 'runtimeVersion')
        booleanFlag(enabled, 'enabled')
        return {
          changed: false,
          application: 'failed',
          stage: 'install',
          target: String(target),
          error: {
            code: 'incompatible-version',
            diagnostic: EXEMPTIONS_DISABLED,
            message: EXEMPTIONS_DISABLED,
          },
        }
      }

      async cancelInstall(): Promise<{ status: 'not-running' }> {
        requireCreator(host, this.ctx, 'pluginManager')
        return { status: 'not-running' }
      }
    }
    Object.freeze(BuiltinPluginManagerService.prototype)
    return BuiltinPluginManagerService
  },
}

export const builtinSandboxPolicyAdapter: DshAdapter = {
  service: 'sandboxPolicy',
  define(host) {
    class BuiltinSandboxPolicyService extends Service {
      constructor(ctx: any) {
        super(ctx, 'sandboxPolicy')
      }

      resolve(_query?: unknown): { mode: 'read-only' } {
        requireCreator(host, this.ctx, 'sandboxPolicy')
        return { mode: 'read-only' }
      }
    }
    Object.freeze(BuiltinSandboxPolicyService.prototype)
    return BuiltinSandboxPolicyService
  },
}

export const builtinApprovalAdapter: DshAdapter = {
  service: 'approval',
  define(host) {
    class BuiltinApprovalService extends Service {
      constructor(ctx: any) {
        super(ctx, 'approval')
      }

      async request(req: unknown): Promise<'allowed-once' | 'rejected' | 'cancelled' | 'unavailable'> {
        if (host.tier !== 'builtin') return 'unavailable'
        if (host.ownerOf(this.ctx)?.ref.plugin_id !== 'host:creator') return 'unavailable'
        const invocation = host.liveInvocation(this.ctx)
        if (invocation === undefined) return 'unavailable'
        if (req === null || typeof req !== 'object' || Array.isArray(req)) return 'unavailable'
        const candidate = req as { agent?: unknown, callId?: unknown, signal?: unknown }
        const signal = candidate.signal
        if (signal !== undefined && !(signal instanceof AbortSignal)) return 'unavailable'
        if (signal?.aborted || invocation.signal.aborted) return 'cancelled'
        if (invocation.agent === undefined || candidate.agent !== invocation.agent || candidate.callId !== invocation.callId) {
          return 'unavailable'
        }
        try {
          const answer = record(await host.broker(this.ctx, 'plugins', 'posture', {}, signal), 'posture')
          const mode = answer.mode
          if (mode === 'read-only') return 'rejected'
          if (mode === 'workspace-write' || mode === 'danger-full-access') return 'allowed-once'
          return 'unavailable'
        } catch (error) {
          if (signal?.aborted || invocation.signal.aborted) return 'cancelled'
          if (error instanceof RpcError && error.code === ErrorCode.Cancelled) return 'cancelled'
          return 'unavailable'
        }
      }
    }
    Object.freeze(BuiltinApprovalService.prototype)
    return BuiltinApprovalService
  },
}

export const BUILTIN_DSH_ADAPTERS: readonly DshAdapter[] = [
  builtinPluginManagerAdapter,
  builtinSandboxPolicyAdapter,
  builtinApprovalAdapter,
]
