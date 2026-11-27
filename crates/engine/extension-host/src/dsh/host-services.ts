import { serialize as v8Serialize } from 'node:v8'
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

const MAX_STORAGE_DOMAINS_PER_OWNER = 16
const MAX_STORAGE_TABLES_PER_DOMAIN = 32
const MAX_STORAGE_ENTRIES_PER_TABLE = 512
const MAX_STORAGE_VALUE_BYTES = 64 * 1024
const MAX_INVARIANTS_PER_OWNER = 64

export const sessionPersistenceAdapter: DshAdapter = {
  service: 'sessionPersistence',
  define() {
    class SessionPersistenceService extends Service {
      constructor(ctx: any) { super(ctx, 'sessionPersistence') }

      async findLog(_id?: unknown): Promise<undefined> {
        return undefined
      }

      async listLogs(): Promise<readonly unknown[]> {
        return Object.freeze([])
      }

      async readLog(_id?: unknown): Promise<undefined> {
        return undefined
      }

      artifactDirectory(_headerOrId?: unknown): undefined {
        return undefined
      }

      async deleteLog(): Promise<never> {
        throw new RpcError(ErrorCode.Refused, 'sessionPersistence mutations are refused in Codewhale')
      }

      async removeLog(): Promise<never> {
        throw new RpcError(ErrorCode.Refused, 'sessionPersistence mutations are refused in Codewhale')
      }

      async writeLog(): Promise<never> {
        throw new RpcError(ErrorCode.Refused, 'sessionPersistence mutations are refused in Codewhale')
      }
    }
    Object.freeze(SessionPersistenceService.prototype)
    return SessionPersistenceService
  },
}

interface ScopedStorageTable {
  get(key: unknown): Promise<unknown>
  set(key: unknown, value: unknown): Promise<void>
  delete(key: unknown): Promise<boolean>
  has(key: unknown): Promise<boolean>
  keys(): Promise<readonly string[]>
  list(): Promise<readonly unknown[]>
  entries(): Promise<readonly [string, unknown][]>
  clear(): Promise<void>
}

interface ScopedStorageDomainHandle {
  readonly name: string
  table(tableName: unknown): ScopedStorageTable
  close(): Promise<void>
}

export const storageDomainAdapter: DshAdapter = {
  service: 'storageDomain',
  define(host: AdapterHost) {
    const ownerDomains = new Map<string, Map<string, Map<string, Map<string, unknown>>>>()

    function makeDomainHandle(token: string, domainName: string, tables: Map<string, Map<string, unknown>>): ScopedStorageDomainHandle {
      let closed = false
      const ensureOpen = () => {
        if (closed || ownerDomains.get(token)?.get(domainName) !== tables) {
          throw new RpcError(ErrorCode.NotAvailable, `storageDomain \`${domainName}\` is closed`)
        }
      }
      return Object.freeze({
        name: domainName,
        table(tableName: unknown): ScopedStorageTable {
          ensureOpen()
          if (typeof tableName !== 'string' || tableName.trim().length === 0 || tableName.length > 128) {
            throw new RpcError(ErrorCode.InvalidParams, 'storageDomain table name must be a non-empty string')
          }
          const key = tableName.trim()
          let rows = tables.get(key)
          if (!rows) {
            if (tables.size >= MAX_STORAGE_TABLES_PER_DOMAIN) {
              throw new RpcError(ErrorCode.Refused, 'storageDomain table limit reached')
            }
            rows = new Map()
            tables.set(key, rows)
          }
          const store = rows
          return Object.freeze({
            async get(rowKey: unknown) {
              ensureOpen()
              if (typeof rowKey !== 'string') return undefined
              const val = store.get(rowKey)
              return val === undefined ? undefined : structuredClone(val)
            },
            async set(rowKey: unknown, value: unknown) {
              ensureOpen()
              if (typeof rowKey !== 'string' || rowKey.length === 0 || rowKey.length > 256) {
                throw new RpcError(ErrorCode.InvalidParams, 'storageDomain row key must be a non-empty string')
              }
              if (!store.has(rowKey) && store.size >= MAX_STORAGE_ENTRIES_PER_TABLE) {
                throw new RpcError(ErrorCode.Refused, 'storageDomain table entry limit reached')
              }
              let serialized: string | undefined
              try {
                serialized = JSON.stringify(value)
              } catch {
                throw new RpcError(ErrorCode.InvalidParams, 'storageDomain value must be JSON-serializable')
              }
              if (serialized === undefined) {
                throw new RpcError(ErrorCode.InvalidParams, 'storageDomain value must be JSON-serializable')
              }
              if (Buffer.byteLength(serialized, 'utf8') > MAX_STORAGE_VALUE_BYTES) {
                throw new RpcError(ErrorCode.InvalidParams, 'storageDomain value exceeds size limit')
              }
              try {
                if (v8Serialize(value).byteLength > MAX_STORAGE_VALUE_BYTES) {
                  throw new RpcError(ErrorCode.InvalidParams, 'storageDomain value exceeds size limit')
                }
              } catch (err) {
                if (err instanceof RpcError) throw err
                throw new RpcError(ErrorCode.InvalidParams, 'storageDomain value must be JSON-serializable')
              }
              store.set(rowKey, JSON.parse(serialized))
            },
            async delete(rowKey: unknown) {
              ensureOpen()
              return typeof rowKey === 'string' ? store.delete(rowKey) : false
            },
            async has(rowKey: unknown) {
              ensureOpen()
              return typeof rowKey === 'string' && store.has(rowKey)
            },
            async keys() {
              ensureOpen()
              return Object.freeze([...store.keys()])
            },
            async list() {
              ensureOpen()
              return Object.freeze([...store.values()].map((v) => structuredClone(v)))
            },
            async entries() {
              ensureOpen()
              return Object.freeze([...store.entries()].map(([k, v]) => [k, structuredClone(v)] as [string, unknown]))
            },
            async clear() {
              ensureOpen()
              store.clear()
            },
          })
        },
        async close() {
          closed = true
          const domains = ownerDomains.get(token)
          if (domains?.get(domainName) === tables) {
            domains.delete(domainName)
            if (domains.size === 0) ownerDomains.delete(token)
          }
        },
      })
    }

    class StorageDomainService extends Service {
      constructor(ctx: any) { super(ctx, 'storageDomain') }

      async open(spec: unknown): Promise<ScopedStorageDomainHandle> {
        const owner = host.ownerOf(this.ctx)
        if (!owner) throw new RpcError(ErrorCode.NotAvailable, 'storageDomain.open requires an active owner')
        const rawName = typeof spec === 'string'
          ? spec
          : spec && typeof spec === 'object' && !Array.isArray(spec)
            ? ((spec as Record<string, unknown>).name ?? (spec as Record<string, unknown>).id)
            : undefined
        if (typeof rawName !== 'string' || rawName.trim().length === 0 || rawName.length > 128) {
          throw new RpcError(ErrorCode.InvalidParams, 'storageDomain spec requires a non-empty name')
        }
        const domainName = rawName.trim()
        const token = owner.ref.owner_token
        let domains = ownerDomains.get(token)
        if (!domains) {
          domains = new Map()
          ownerDomains.set(token, domains)
        }
        let tables = domains.get(domainName)
        if (!tables) {
          if (domains.size >= MAX_STORAGE_DOMAINS_PER_OWNER) {
            throw new RpcError(ErrorCode.Refused, 'storageDomain owner domain limit reached')
          }
          tables = new Map()
          domains.set(domainName, tables)
          const ownedTables = tables
          ;(this.ctx as any).effect(() => () => {
            const current = ownerDomains.get(token)
            if (current?.get(domainName) === ownedTables) {
              current.delete(domainName)
              if (current.size === 0) ownerDomains.delete(token)
            }
          }, `storageDomain.open(${JSON.stringify(domainName)})`)
        }
        return makeDomainHandle(token, domainName, tables)
      }

      get(name: unknown): ScopedStorageDomainHandle | undefined {
        if (typeof name !== 'string' || name.trim().length === 0) return undefined
        const owner = host.ownerOf(this.ctx)
        if (!owner) return undefined
        const token = owner.ref.owner_token
        const tables = ownerDomains.get(token)?.get(name.trim())
        return tables ? makeDomainHandle(token, name.trim(), tables) : undefined
      }

      list(): readonly string[] {
        const owner = host.ownerOf(this.ctx)
        const domains = owner ? ownerDomains.get(owner.ref.owner_token) : undefined
        return Object.freeze(domains ? [...domains.keys()] : [])
      }
    }
    Object.freeze(StorageDomainService.prototype)
    return StorageDomainService
  },
}

export const invariantsAdapter: DshAdapter = {
  service: 'invariants',
  define(host: AdapterHost) {
    const ownerInvariants = new Map<string, Set<string>>()
    class InvariantsService extends Service {
      constructor(ctx: any) { super(ctx, 'invariants') }

      register(name: unknown, _install?: unknown): () => void {
        const owner = host.ownerOf(this.ctx)
        if (!owner) throw new RpcError(ErrorCode.NotAvailable, 'invariants.register requires an active owner')
        if (typeof name !== 'string' || name.trim().length === 0 || name.length > 128) {
          throw new RpcError(ErrorCode.InvalidParams, 'invariant name must be a non-empty string')
        }
        const key = name.trim()
        const token = owner.ref.owner_token
        const existing = ownerInvariants.get(token)
        if (existing && !existing.has(key) && existing.size >= MAX_INVARIANTS_PER_OWNER) {
          throw new RpcError(ErrorCode.Refused, 'invariants owner limit reached')
        }
        return (this.ctx as any).effect(() => {
          let names = ownerInvariants.get(token)
          if (!names) {
            names = new Set()
            ownerInvariants.set(token, names)
          }
          if (!names.has(key) && names.size >= MAX_INVARIANTS_PER_OWNER) {
            throw new RpcError(ErrorCode.Refused, 'invariants owner limit reached')
          }
          names.add(key)
          return () => {
            const current = ownerInvariants.get(token)
            current?.delete(key)
            if (current && current.size === 0) ownerInvariants.delete(token)
          }
        }, `invariants.register(${JSON.stringify(key)})`)
      }

      list(): readonly string[] {
        const owner = host.ownerOf(this.ctx)
        const names = owner ? ownerInvariants.get(owner.ref.owner_token) : undefined
        return Object.freeze(names ? [...names] : [])
      }
    }
    Object.freeze(InvariantsService.prototype)
    return InvariantsService
  },
}

const READONLY_PERMISSION_PRESET_NAMES = Object.freeze(['ask', 'accept-edits', 'auto-review', 'full-access'] as const)
const READONLY_PERMISSION_PRESETS = Object.freeze(
  READONLY_PERMISSION_PRESET_NAMES.map((name) => Object.freeze({ name, readonly: true })),
)

export const permissionPresetsAdapter: DshAdapter = {
  service: 'permissionPresets',
  define() {
    class PermissionPresetsService extends Service {
      constructor(ctx: any) { super(ctx, 'permissionPresets') }

      get availability(): 'readonly' {
        return 'readonly'
      }

      get defaultPreset(): string {
        return 'ask'
      }

      get names(): readonly string[] {
        return READONLY_PERMISSION_PRESET_NAMES
      }

      list(): readonly Readonly<{ name: string, readonly: boolean }>[] {
        return READONLY_PERMISSION_PRESETS
      }

      get(name: unknown): Readonly<{ name: string, readonly: boolean }> | undefined {
        if (typeof name !== 'string') return undefined
        return READONLY_PERMISSION_PRESETS.find((preset) => preset.name === name)
      }

      apply(): never {
        throw new RpcError(ErrorCode.Refused, 'permissionPresets is read-only in Codewhale; Core owns approval posture')
      }

      set(): never {
        throw new RpcError(ErrorCode.Refused, 'permissionPresets is read-only in Codewhale; Core owns approval posture')
      }

      register(): never {
        throw new RpcError(ErrorCode.Refused, 'permissionPresets is read-only in Codewhale; Core owns approval posture')
      }
    }
    Object.freeze(PermissionPresetsService.prototype)
    return PermissionPresetsService
  },
}

export const DSH_HOST_SERVICE_ADAPTERS: readonly DshAdapter[] = Object.freeze([
  agentPresetsAdapter,
  agentDefaultModelAdapter,
  sessionProjectionsAdapter,
  workspaceRegistryAdapter,
  typertGatewayAdapter,
  webAdapter,
  sessionPersistenceAdapter,
  storageDomainAdapter,
  invariantsAdapter,
  permissionPresetsAdapter,
])
