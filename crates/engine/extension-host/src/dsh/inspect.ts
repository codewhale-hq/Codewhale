import { isJson } from '../json.ts'
import { assertSupportedJsonSchema, validateJsonSchemaValue } from './dsh-tools-compat.js'
import catalogData from './inspect-catalog.json' with { type: 'json' }

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue }
export type InspectPlatform = 'host' | 'client'

export interface InspectMethod {
  name: string
  description: string
  inputSchema: JsonValue
  outputSchema: JsonValue
}

export interface InspectProviderView {
  platform: InspectPlatform
  id: string
  description: string
  methods: InspectMethod[]
}

export interface InstalledRow {
  id: string
  name: string
  state: string
}

export interface ClientState {
  connected: boolean
  drawnSeats?: readonly string[]
}

export interface InspectSources {
  tools?: (agent: unknown, signal: AbortSignal) => Promise<readonly JsonValue[]> | readonly JsonValue[]
  installed?: (signal: AbortSignal) => Promise<readonly InstalledRow[]> | readonly InstalledRow[]
  client?: () => ClientState
}

export interface CordisInspect {
  list(): InspectProviderView[]
  query(platform: string, provider: string, method: string, input: JsonValue | undefined, agent?: unknown, signal?: AbortSignal): Promise<JsonValue>
}

interface Provider {
  platform: InspectPlatform
  id: string
  description: string
  methods: InspectMethod[]
  run(method: string, input: Record<string, JsonValue>, agent: unknown, signal: AbortSignal): Promise<any> | any
}

const catalog = catalogData as any
const EMPTY_INPUT: JsonValue = { type: 'object', properties: {}, additionalProperties: false }
const ANY_OUTPUT: JsonValue = { description: 'JSON data owned by this inspect provider.' }
const NO_CLIENT = 'No client is connected. Plugin screens are drawn by the desktop app; in a terminal nothing is drawn, so every slot reports drawn: false.'

const exactInput = (field: string, description: string): JsonValue => ({
  type: 'object',
  properties: { [field]: { type: 'string', description } },
  additionalProperties: false,
})

const method = (name: string, description: string, inputSchema: JsonValue = EMPTY_INPUT, outputSchema: JsonValue = ANY_OUTPUT): InspectMethod =>
  ({ name, description, inputSchema, outputSchema })

const copy = <T>(value: T): T => JSON.parse(JSON.stringify(value))

function serviceView(service: any): JsonValue {
  return {
    key: service.key,
    group: service.group,
    summary: service.summary,
    availability: service.availability,
    availabilityText: service.availabilityText,
    methods: service.methods,
  }
}

function serviceProvider(): Provider {
  return {
    platform: 'host', id: 'Service',
    description: 'The services this host gives plugins, with their methods, and the services a plugin may not provide. A name that is not listed does not exist here.',
    methods: [method('listService', 'Without input: the directory of services with one-line summaries and method signatures. With service: that service\'s exact contract, with availability. These are services a plugin injects, not DSH\'s full catalogue.',
      exactInput('service', 'Exact service key. Omit it for the directory.'))],
    run(_name, input) {
      const key = input.service
      if (key === undefined) {
        return {
          services: catalog.host.services.map((s: any) => ({ key: s.key, group: s.group, summary: s.summary, availability: s.availability, signatures: s.methods.map((m: any) => m.signature) })),
          refusedServices: catalog.host.refusedServices,
          note: 'refusedServices are names a plugin may not provide: the Engine owns them. Service methods named by the host Event and Config providers are separate.',
          targetDshVersion: catalog.targetDshVersion,
        }
      }
      const found = catalog.host.services.find((s: any) => s.key === key)
      if (!found) throw new Error(`Host service "${String(key)}" is not provided by this host. Call without service for the directory.`)
      return { service: serviceView(found), targetDshVersion: catalog.targetDshVersion }
    },
  }
}

function eventProvider(): Provider {
  return {
    platform: 'host', id: 'Event',
    description: 'The events a plugin may listen to in this host, and the ones the host refuses.',
    methods: [method('listEvents', 'Without input: the directory of events with status supported or refused. With event: its exact contract.',
      exactInput('event', 'Exact event name. Omit it for the directory.'))],
    run(_name, input) {
      const key = input.event
      if (key === undefined) {
        return {
          events: catalog.host.events.map((e: any) => ({ name: e.name, status: e.status, summary: e.summary })),
          note: catalog.host.cordisInternalEvents,
        }
      }
      const found = catalog.host.events.find((e: any) => e.name === key)
      if (!found) throw new Error(`Host event "${String(key)}" is not known to this host. Call without event for the directory.`)
      return { event: found }
    },
  }
}

const CONFIG_INPUT: JsonValue = {
  type: 'object',
  properties: {
    entry: { type: 'string', description: 'Exact entry id from the directory; returns that entry\'s Config schema.' },
    name: { type: 'string', description: 'Exact package or plugin name; limits the directory to it.' },
    offset: { type: 'integer', description: 'Zero-based directory offset; defaults to 0.' },
    limit: { type: 'integer', description: 'Directory page size, from 1 to 100; defaults to 25.' },
  },
  additionalProperties: false,
}

function configProvider(sources: InspectSources): Provider {
  return {
    platform: 'host', id: 'Config',
    description: 'Config a composition row may carry: the packages a row may name, each with the config it accepts, and the plugins installed on this host with their state.',
    methods: [method('listConfigs', 'Without entry: a page of the directory (row packages first, then installed plugins) with total and nextOffset. With entry: one row package\'s exact JSON Schema, limits and a valid example. Installed plugins are listed with their state; their own config is not described here.', CONFIG_INPUT)],
    async run(_name, input, _agent, signal) {
      const rows: any[] = catalog.host.config.map((r: any) => ({ id: r.id, name: r.name, kind: 'row-package', status: 'schema', bridge: r.bridge, summary: r.summary }))
      const installed = sources.installed ? await sources.installed(signal) : []
      for (const row of installed) rows.push({ id: `plugin:${row.id}`, name: row.name, kind: 'installed-plugin', status: row.state })
      if (input.entry !== undefined) {
        const pkg = catalog.host.config.find((r: any) => r.id === input.entry)
        if (pkg) return { entry: pkg.id, name: pkg.name, kind: 'row-package', bridge: pkg.bridge, summary: pkg.summary, limits: pkg.limits, example: pkg.example, ...pkg.hookEvents ? { hookEvents: pkg.hookEvents } : {}, schema: pkg.schema }
        const live = rows.find((r) => r.id === input.entry)
        if (live) return { entry: live.id, name: live.name, kind: live.kind, status: live.status, schema: null, note: 'An installed plugin\'s own Config is not described by this host.' }
        throw new Error(`Host Config entry "${String(input.entry)}" is not listed. Call without entry for the directory.`)
      }
      const filtered = input.name === undefined ? rows : rows.filter((r) => r.name === input.name)
      const offset = typeof input.offset === 'number' ? input.offset : 0
      const limit = typeof input.limit === 'number' ? input.limit : 25
      if (offset < 0 || limit < 1 || limit > 100) throw new Error('Host Config directory needs offset >= 0 and 1 <= limit <= 100')
      const page = filtered.slice(offset, offset + limit)
      const next = offset + page.length
      return { entries: page, total: filtered.length, ...next < filtered.length ? { nextOffset: next } : {} }
    },
  }
}

function toolProvider(sources: InspectSources): Provider | undefined {
  const source = sources.tools
  if (!source) return undefined
  return {
    platform: 'host', id: 'Tool',
    description: 'Tools visible to the requesting agent.',
    methods: [method('listTools', 'Return every tool schema currently callable by this agent.')],
    async run(_name, _input, agent, signal) {
      return { tools: [...await source(agent, signal)] }
    },
  }
}

const SUBTREE_INPUT = exactInput('root', 'Exact slot name to select. Omit it for every slot.')

function slotsProvider(sources: InspectSources): Provider {
  return {
    platform: 'client', id: 'Slots',
    description: 'The DSH slot names this host accepts, the seat each maps to, and whether the connected client draws that seat.',
    methods: [method('listSubTree', 'Return every slot with its seat and drawn state; with root, also that slot selected. Slots are static names here, not a live tree, and their props are not described by this host: a plugin registers on a slot, the Engine serves it on the mapped seat, and the client draws only the seats it places.', SUBTREE_INPUT)],
    run(_name, input) {
      const state = sources.client?.() ?? { connected: false }
      const drawn = new Set(state.connected ? state.drawnSeats ?? catalog.client.seats.filter((s: any) => s.drawn).map((s: any) => s.seat) : [])
      const slots = catalog.client.slots.map((s: any) => ({
        type: 'slot', name: s.slot, seat: s.seat, supported: s.supported,
        drawn: s.supported && drawn.has(s.seat),
        ...s.reason ? { reason: s.reason } : {},
      }))
      const seats = catalog.client.seats.map((s: any) => ({ seat: s.seat, drawn: drawn.has(s.seat), where: s.where }))
      const out: Record<string, JsonValue> = {
        client: state.connected ? { connected: true } : { connected: false, message: NO_CLIENT },
        seats, trees: slots, referencedTypes: [],
      }
      if (input.root !== undefined) {
        const selected = slots.find((s: any) => s.name === input.root)
        out.requestedRoot = { name: input.root, available: selected !== undefined }
        if (selected) out.selected = selected
      }
      return out
    },
  }
}

function themeProvider(): Provider {
  return {
    platform: 'client', id: 'Theme',
    description: 'What the plugin frame provides for theming.',
    methods: [method('listTokens', 'Return the named tokens the frame provides (none), the colour schemes, and the theme calls a plugin has.')],
    run() {
      return { tokens: catalog.client.theme.tokens, colorSchemes: catalog.client.theme.colorSchemes, scheme: catalog.client.theme.scheme, api: catalog.client.theme.api, notes: catalog.client.theme.notes, referencedTypes: [] }
    },
  }
}

export function createCordisInspect(sources: InspectSources = {}): CordisInspect {
  const providers: Provider[] = [serviceProvider(), eventProvider(), configProvider(sources), toolProvider(sources), slotsProvider(sources), themeProvider()]
    .filter((p): p is Provider => p !== undefined)
  for (const provider of providers) {
    if (provider.id.trim() === '' || provider.description.trim() === '') throw new Error('Cordis inspect provider needs an id and a description')
    for (const m of provider.methods) {
      assertSupportedJsonSchema(m.inputSchema)
      assertSupportedJsonSchema(m.outputSchema)
    }
  }
  const label = (platform: InspectPlatform) => platform === 'host' ? 'Host' : 'Client'
  return {
    list() {
      return providers.map((p) => ({ platform: p.platform, id: p.id, description: p.description, methods: copy(p.methods) }))
    },
    async query(platform, providerId, methodName, input, agent, signal = new AbortController().signal) {
      if (platform !== 'host' && platform !== 'client') throw new Error(`Cordis inspect platform must be "host" or "client", not "${String(platform)}"`)
      const provider = providers.find((p) => p.platform === platform && p.id === providerId)
      if (!provider) throw new Error(`${label(platform)} Cordis inspect provider "${providerId}" is not registered`)
      const found = provider.methods.find((m) => m.name === methodName)
      if (!found) throw new Error(`Cordis inspect provider "${provider.id}" has no method "${methodName}"`)
      const violations = validateJsonSchemaValue(found.inputSchema, input ?? {}, 'input') as string[]
      if (violations.length > 0) throw new Error(`${label(platform)} Cordis inspect ${provider.id}.${found.name} rejected input: ${violations.join('; ')}`)
      signal.throwIfAborted()
      const data = await provider.run(methodName, (input ?? {}) as Record<string, JsonValue>, agent, signal)
      signal.throwIfAborted()
      if (!isJson(data)) throw new Error(`${label(platform)} Cordis inspect ${provider.id}.${found.name} returned a non-JSON value`)
      return copy(data)
    },
  }
}
