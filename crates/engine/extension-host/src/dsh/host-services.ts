import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import { brokeredFetch } from './network.ts'
import type { AdapterHost, DshAdapter } from './adapters.ts'

export interface RegisteredSearchProvider {
  readonly id: string
  readonly name: string
  readonly search?: (...args: unknown[]) => unknown
}

export const agentPresetsAdapter: DshAdapter = {
  service: 'agentPresets',
  define(host: AdapterHost) {
    const ownerPresets = new Map<string, Map<string, Record<string, unknown>>>()
    class AgentPresetsService extends Service {
      constructor(ctx: any) { super(ctx, 'agentPresets') }

      get defaultId(): string | undefined { return undefined }
      get authorable(): boolean { return false }

      composedPreset(): string | undefined { return undefined }

      async list(): Promise<Record<string, unknown>[]> {
        const owner = host.ownerOf(this.ctx)
        const table = owner ? ownerPresets.get(owner.ref.owner_token) : undefined
        return table ? [...table.values()].map((row) => structuredClone(row)) : []
      }

      async get(id: unknown): Promise<Record<string, unknown> | undefined> {
        if (typeof id !== 'string' || id.length === 0) return undefined
        const rows = await this.list()
        return rows.find((row) => row.id === id)
      }

      async resolve(id?: string): Promise<Record<string, unknown>> {
        const found = await this.get(id ?? this.defaultId)
        if (!found) throw new RpcError(ErrorCode.NotAvailable, 'preset not found')
        return found
      }

      async remoteExportList() {
        const presets = await this.list()
        return { presets, authorable: false, defaultId: undefined, modeSelectionEnabled: false }
      }

      async compositionInventory() {
        return []
      }

      register(preset: unknown): () => void {
        const owner = host.ownerOf(this.ctx)
        if (!owner) throw new RpcError(ErrorCode.NotAvailable, 'agentPresets.register requires an active owner')
        if (!preset || typeof preset !== 'object' || Array.isArray(preset)) {
          throw new RpcError(ErrorCode.InvalidParams, 'preset must be an object')
        }
        const record = preset as Record<string, unknown>
        const id = typeof record.id === 'string' && record.id.trim().length > 0 ? record.id.trim() : undefined
        if (!id) throw new RpcError(ErrorCode.InvalidParams, 'preset.id must be a non-empty string')
        const token = owner.ref.owner_token
        return (this.ctx as any).effect(() => {
          let table = ownerPresets.get(token)
          if (!table) {
            table = new Map()
            ownerPresets.set(token, table)
          }
          table.set(id, structuredClone(record))
          return () => {
            const current = ownerPresets.get(token)
            current?.delete(id)
            if (current && current.size === 0) ownerPresets.delete(token)
          }
        }, `agentPresets.register(${JSON.stringify(id)})`)
      }

      mount(): never {
        throw new RpcError(ErrorCode.NotAvailable, 'select or mount presets through Core agent preset selection')
      }
      select(): never {
        throw new RpcError(ErrorCode.NotAvailable, 'select through Core AgentProfile/native preset selection before a turn')
      }
    }
    Object.freeze(AgentPresetsService.prototype)
    return AgentPresetsService
  },
}

export const agentDefaultModelAdapter: DshAdapter = {
  service: 'agentDefaultModel',
  define(host: AdapterHost) {
    class AgentDefaultModelService extends Service {
      constructor(ctx: any) { super(ctx, 'agentDefaultModel') }

      currentSelection(): Readonly<{ provider: string, model: string }> | undefined {
        const invocation = host.liveInvocation(this.ctx)
        if (!invocation) return undefined
        return Object.freeze({ provider: 'default', model: 'default' })
      }
    }
    Object.freeze(AgentDefaultModelService.prototype)
    return AgentDefaultModelService
  },
}

export const sessionProjectionsAdapter: DshAdapter = {
  service: 'sessionProjections',
  define(host: AdapterHost) {
    class SessionProjectionsService extends Service {
      constructor(ctx: any) { super(ctx, 'sessionProjections') }

      get(sessionId?: unknown) {
        const invocation = host.liveInvocation(this.ctx)
        const activeId = (invocation?.agent as { session?: { id?: string }, id?: string } | undefined)?.session?.id
          ?? (invocation?.agent as { id?: string } | undefined)?.id
        if (!activeId) return undefined
        if (sessionId !== undefined && sessionId !== activeId) return undefined
        return Object.freeze({
          sessionId: activeId,
          messages: Object.freeze([]),
          events: Object.freeze([]),
        })
      }

      list() {
        const current = this.get()
        return current ? Object.freeze([current]) : Object.freeze([])
      }

      messages(sessionId?: unknown) {
        return this.get(sessionId)?.messages ?? Object.freeze([])
      }

      events(sessionId?: unknown) {
        return this.get(sessionId)?.events ?? Object.freeze([])
      }
    }
    Object.freeze(SessionProjectionsService.prototype)
    return SessionProjectionsService
  },
}

export const workspaceRegistryAdapter: DshAdapter = {
  service: 'workspaceRegistry',
  define(host: AdapterHost) {
    function currentWorkspace(ctx: unknown) {
      const invocation = host.liveInvocation(ctx)
      const cwd = (invocation?.session as { header?: { cwd?: string } } | undefined)?.header?.cwd
      if (!cwd) return undefined
      return Object.freeze({
        id: cwd,
        path: cwd,
        status: async () => 'available' as const,
      })
    }
    class WorkspaceRegistryService extends Service {
      constructor(ctx: any) { super(ctx, 'workspaceRegistry') }

      get state() {
        return Object.freeze({ archivedSessionIds: Object.freeze([]) })
      }

      list() {
        const ws = currentWorkspace(this.ctx)
        return ws ? Object.freeze([ws]) : Object.freeze([])
      }

      get(id: unknown) {
        const ws = currentWorkspace(this.ctx)
        if (!ws || typeof id !== 'string') return undefined
        return ws.id === id || ws.path === id ? ws : undefined
      }

      resolveByPath(path: unknown) {
        const ws = currentWorkspace(this.ctx)
        if (!ws || typeof path !== 'string') return undefined
        return ws.path === path ? ws : undefined
      }

      create(): never {
        throw new RpcError(ErrorCode.Refused, 'workspaceRegistry is read-only in Codewhale')
      }

      delete(): never {
        throw new RpcError(ErrorCode.Refused, 'workspaceRegistry is read-only in Codewhale')
      }

      enqueueOperation(): never {
        throw new RpcError(ErrorCode.Refused, 'workspaceRegistry mutations are refused in Codewhale')
      }
    }
    Object.freeze(WorkspaceRegistryService.prototype)
    return WorkspaceRegistryService
  },
}

export const typertGatewayAdapter: DshAdapter = {
  service: 'typertGateway',
  define(host: AdapterHost) {
    const ownerServices = new Map<string, Map<string, { service: unknown, descriptors: readonly unknown[] }>>()
    class TypertGatewayService extends Service {
      constructor(ctx: any) { super(ctx, 'typertGateway') }

      register(name: unknown, service: unknown, descriptors: readonly unknown[] = []): () => void {
        const owner = host.ownerOf(this.ctx)
        if (!owner) throw new RpcError(ErrorCode.NotAvailable, 'typertGateway.register requires an active owner')
        if (typeof name !== 'string' || name.trim().length === 0 || name.length > 128) {
          throw new RpcError(ErrorCode.InvalidParams, 'typertGateway service name must be a non-empty string')
        }
        const key = name.trim()
        const token = owner.ref.owner_token
        const frozenDescriptors = Object.freeze(Array.isArray(descriptors) ? [...descriptors] : [])
        return (this.ctx as any).effect(() => {
          let table = ownerServices.get(token)
          if (!table) {
            table = new Map()
            ownerServices.set(token, table)
          }
          table.set(key, { service, descriptors: frozenDescriptors })
          return () => {
            const current = ownerServices.get(token)
            current?.delete(key)
            if (current && current.size === 0) ownerServices.delete(token)
          }
        }, `typertGateway.register(${JSON.stringify(key)})`)
      }

      list(): readonly string[] {
        const owner = host.ownerOf(this.ctx)
        const table = owner ? ownerServices.get(owner.ref.owner_token) : undefined
        return Object.freeze(table ? [...table.keys()] : [])
      }

      get(name: unknown) {
        if (typeof name !== 'string') return undefined
        const owner = host.ownerOf(this.ctx)
        const table = owner ? ownerServices.get(owner.ref.owner_token) : undefined
        return table?.get(name)
      }
    }
    Object.freeze(TypertGatewayService.prototype)
    return TypertGatewayService
  },
}

export const webAdapter: DshAdapter = {
  service: 'web',
  define(host: AdapterHost) {
    const ownerProviders = new Map<string, Map<string, RegisteredSearchProvider>>()
    class WebService extends Service {
      constructor(ctx: any) { super(ctx, 'web') }

      fetch(input: string | URL | Request, init?: RequestInit): Promise<Response> {
        return brokeredFetch(host, this.ctx, input, init)
      }

      registerSearchProvider(provider: unknown): () => void {
        const owner = host.ownerOf(this.ctx)
        if (!owner) throw new RpcError(ErrorCode.NotAvailable, 'web.registerSearchProvider requires an active owner')
        if (!provider || typeof provider !== 'object' || Array.isArray(provider)) {
          throw new RpcError(ErrorCode.InvalidParams, 'search provider must be an object')
        }
        const row = provider as Record<string, unknown>
        const id = typeof row.id === 'string' && row.id.trim().length > 0
          ? row.id.trim()
          : typeof row.name === 'string' && row.name.trim().length > 0
            ? row.name.trim()
            : undefined
        if (!id) throw new RpcError(ErrorCode.InvalidParams, 'search provider requires a non-empty id or name')
        const token = owner.ref.owner_token
        const entry: RegisteredSearchProvider = Object.freeze({
          id,
          name: typeof row.name === 'string' && row.name.trim().length > 0 ? row.name.trim() : id,
          ...(typeof row.search === 'function' ? { search: row.search as (...args: unknown[]) => unknown } : {}),
        })
        return (this.ctx as any).effect(() => {
          let table = ownerProviders.get(token)
          if (!table) {
            table = new Map()
            ownerProviders.set(token, table)
          }
          table.set(id, entry)
          return () => {
            const current = ownerProviders.get(token)
            current?.delete(id)
            if (current && current.size === 0) ownerProviders.delete(token)
          }
        }, `web.registerSearchProvider(${JSON.stringify(id)})`)
      }

      listSearchProviders(): readonly RegisteredSearchProvider[] {
        const owner = host.ownerOf(this.ctx)
        const table = owner ? ownerProviders.get(owner.ref.owner_token) : undefined
        return Object.freeze(table ? [...table.values()] : [])
      }
    }
    Object.freeze(WebService.prototype)
    return WebService
  },
}

export const DSH_HOST_SERVICE_ADAPTERS: readonly DshAdapter[] = Object.freeze([
  agentPresetsAdapter,
  agentDefaultModelAdapter,
  sessionProjectionsAdapter,
  workspaceRegistryAdapter,
  typertGatewayAdapter,
  webAdapter,
])
