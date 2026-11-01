/** Tier-0 SDK protocol owner. Rust owns launch, every operation grant,
 * pipes, catalog admission, keys, credentials, cancellation and teardown. */
import { Client, SSEClientTransport, StreamableHTTPClientTransport, parseJSONRPCMessage, type JSONRPCMessage, type RequestMethod, type Transport } from '@modelcontextprotocol/client'
import type { OwnerRef } from '../protocol.generated.ts'

export interface BrokerRpc {
  request<T>(method: string, params: unknown, signal?: AbortSignal): Promise<T>
}
export interface OperationGrant {
  ticket: string
  operation_id: string
  method: string
  wire_id?: string
  params: Record<string, unknown>
}
export interface OpenParams {
  owner: OwnerRef
  session_id: string
  launch_ticket: string
  transport?: 'stdio' | 'http' | 'sse'
  discover_grant?: OperationGrant
  initialize_grant: OperationGrant
  initialized_grant: OperationGrant
  client_version: string
  deadline_ms: number
}
export interface RequestParams {
  owner: OwnerRef
  session_id: string
  grant: OperationGrant
  deadline_ms: number
}
const MAX_FRAME_BYTES = 16 * 1024 * 1024
const MAX_SESSIONS = 64
const MAX_PENDING = 256
const MAX_DEPTH = 128
function requestMethod(value: string): RequestMethod {
  switch (value) {
    case 'tools/list': case 'resources/list': case 'resources/templates/list': case 'prompts/list':
    case 'tools/call': case 'resources/read': case 'prompts/get': return value
    default: throw new Error('MCP request method is unavailable')
  }
}
const EXTENSION_METHODS = new Set(['skills/list', 'skills/get'])
const brokerValidated = { '~standard': { version: 1, vendor: 'codewhale', validate: (value: unknown) => ({ value }) } } as const
const SUPPORTED = ['2026-07-28', '2025-06-18', '2025-11-25', '2025-03-26', '2024-11-05']
function identity(owner: OwnerRef): string {
  return JSON.stringify([owner.plugin_id, owner.generation, owner.owner_token])
}
function jsonEqual(a: unknown, b: unknown, depth = 0): boolean {
  if (depth > MAX_DEPTH) throw new Error('MCP operation nesting exceeds the bound')
  if (a === b) return true
  if (typeof a !== typeof b || a === null || b === null) return false
  if (Array.isArray(a) || Array.isArray(b)) {
    return Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((value, i) => jsonEqual(value, b[i], depth + 1))
  }
  if (typeof a !== 'object') return false
  const first = Object.keys(a as object), second = Object.keys(b as object)
  if (first.length !== second.length) return false
  return first.every(key => Object.hasOwn(b as object, key) && jsonEqual((a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key], depth + 1))
}
function frameSize(value: unknown): number {
  return Buffer.byteLength(JSON.stringify(value), 'utf8')
}
function deadline(value: number): number {
  if (!Number.isSafeInteger(value) || value < 1 || value > 24 * 60 * 60 * 1000) throw new Error('invalid MCP request deadline')
  return value
}

interface AdmittedFrame { frame: JSONRPCMessage; ticket?: string; operation_id?: string }
/** Real SDK IDs stay internal. Rust-issued string IDs and exact grants reach peers. */
class OperationFrames {
  private readonly grants = new Map<string, OperationGrant>()
  private readonly pending = new Map<string, { operation: string; wire: string; sdk: string | number }>()
  private readonly wireToSdk = new Map<string, string>()
  private readonly replied = new Set<string>()
  grant(value: OperationGrant): void {
    if (this.grants.size >= MAX_PENDING || this.grants.has(value.method)) throw new Error('MCP grant slot unavailable')
    if (value.wire_id !== undefined && (!value.wire_id || value.wire_id.length > 256)) throw new Error('invalid Rust wire ID')
    this.grants.set(value.method, structuredClone(value))
  }
  incoming(value: unknown): JSONRPCMessage {
    const frame = parseJSONRPCMessage(value)
    if ('id' in frame && !('method' in frame)) {
      const sdkKey = this.wireToSdk.get(JSON.stringify(frame.id))
      if (sdkKey === undefined) throw new Error('MCP reply has no admitted wire ID')
      const binding = this.pending.get(sdkKey)!
      this.pending.delete(sdkKey); this.wireToSdk.delete(JSON.stringify(frame.id))
      if (this.replied.size >= MAX_PENDING) throw new Error('MCP completed exchange bound exceeded')
      this.replied.add(binding.operation)
      frame.id = binding.sdk
    }
    return frame
  }
  outgoing(value: JSONRPCMessage): AdmittedFrame {
    const frame = structuredClone(value)
    if (!('method' in frame)) {
      const refusal = 'error' in frame && frame.error.code === -32601
      const ping = 'result' in frame && Object.keys(frame.result).length === 0
      if (!refusal && !ping) throw new Error('MCP server request requires Rust authority')
      return { frame }
    }
    if (frame.method === 'notifications/cancelled') {
      const params = frame.params as Record<string, unknown> | undefined
      const binding = this.pending.get(JSON.stringify(params?.requestId))
      if (!binding || !params) throw new Error('MCP cancellation has no admitted request')
      params.requestId = binding.wire
      return { frame, operation_id: binding.operation }
    }
    const grant = this.grants.get(frame.method)
    if (!grant || !jsonEqual(frame.params ?? {}, grant.params)) throw new Error('MCP frame has no exact operation grant')
    if ('id' in frame) {
      if (grant.wire_id === undefined || this.pending.size >= MAX_PENDING || this.wireToSdk.has(JSON.stringify(grant.wire_id))) throw new Error('MCP wire ID is not admitted')
      const sdk = frame.id, key = JSON.stringify(sdk)
      if (this.pending.has(key)) throw new Error('duplicate SDK request ID')
      this.pending.set(key, { operation: grant.operation_id, wire: grant.wire_id, sdk })
      this.wireToSdk.set(JSON.stringify(grant.wire_id), key)
      frame.id = grant.wire_id
    } else if (grant.wire_id !== undefined) throw new Error('notification cannot redeem a request grant')
    this.grants.delete(frame.method)
    return { frame, ticket: grant.ticket, operation_id: grant.operation_id }
  }
  takeReply(operation: string): boolean { const found = this.replied.has(operation); this.replied.delete(operation); return found }
  clear(): void { this.grants.clear(); this.pending.clear(); this.wireToSdk.clear(); this.replied.clear() }
}
interface GrantedTransport extends Transport { grant(value: OperationGrant): void; takeReply(operation: string): boolean }
class BrokerTransport implements GrantedTransport {
  onclose?: () => void
  onerror?: (error: Error) => void
  onmessage?: Transport['onmessage']
  private readonly stop = new AbortController()
  private readonly frames = new OperationFrames()
  private started = false
  private closed = false
  constructor(private readonly rpc: BrokerRpc, private readonly owner: OwnerRef, private readonly session: string, private readonly launch: string) {}
  takeReply(operation: string): boolean { return this.frames.takeReply(operation) }
  grant(value: OperationGrant): void { if (this.closed) throw new Error('MCP transport closed'); this.frames.grant(value) }
  async start(): Promise<void> {
    if (this.started || this.closed) throw new Error('MCP transport already started or closed')
    this.started = true
    await this.rpc.request('proc/launch', { owner: this.owner, session_id: this.session, ticket: this.launch }, this.stop.signal)
    void this.readLoop().catch(async error => { if (!this.closed) this.onerror?.(error); await this.close().catch(() => undefined) })
  }
  private async readLoop(): Promise<void> {
    while (!this.closed) {
      const result = await this.rpc.request<{ frame?: unknown; closed?: boolean }>('proc/read', { owner: this.owner, session_id: this.session }, this.stop.signal)
      if (result.closed) { this.finish(); return }
      if (result.frame === undefined || frameSize(result.frame) > MAX_FRAME_BYTES) throw new Error('MCP broker frame exceeds the bound')
      this.onmessage?.(this.frames.incoming(result.frame))
    }
  }
  async send(message: JSONRPCMessage): Promise<void> {
    if (this.closed || !this.started || frameSize(message) > MAX_FRAME_BYTES) throw new Error('MCP broker write unavailable')
    const { frame, ticket, operation_id } = this.frames.outgoing(message)
    try {
      await this.rpc.request('proc/write', { owner: this.owner, session_id: this.session, frame, ...(ticket === undefined ? {} : { ticket }), ...(operation_id === undefined ? {} : { operation_id }) }, this.stop.signal)
    } catch (error) { await this.close().catch(() => undefined); throw error }
  }
  private finish(): void { if (this.closed) return; this.closed = true; this.stop.abort(); this.frames.clear(); this.onclose?.() }
  async close(): Promise<void> { if (this.closed) return; this.finish(); await this.rpc.request('proc/close', { owner: this.owner, session_id: this.session }) }
}
/** Official HTTP/SSE framing with an opaque Rust-authorized FetchProxy. */
class HttpBrokerTransport implements GrantedTransport {
  onclose?: () => void
  onerror?: (error: Error) => void
  onmessage?: Transport['onmessage']
  private readonly stop = new AbortController()
  private readonly frames = new OperationFrames()
  private readonly writes = new Map<string, AdmittedFrame>()
  private inner: SSEClientTransport | StreamableHTTPClientTransport
  private negotiation?: OperationGrant
  private legacyHandshake = false
  private legacy: boolean
  private started = false
  private closed = false
  constructor(private readonly rpc: BrokerRpc, private readonly owner: OwnerRef, private readonly session: string, private readonly launch: string, legacy: boolean) {
    this.legacy = legacy
    this.inner = this.createInner(legacy)
  }
  private createInner(legacy: boolean): SSEClientTransport | StreamableHTTPClientTransport {
    const url = new URL(`https://mcp-proxy.invalid/${this.session}`)
    const fetcher = this.fetch.bind(this)
    const inner = legacy ? new SSEClientTransport(url, { fetch: fetcher }) : new StreamableHTTPClientTransport(url, { fetch: fetcher, onInsufficientScope: 'throw', reconnectionOptions: { maxRetries: 0, initialReconnectionDelay: 0, maxReconnectionDelay: 0, reconnectionDelayGrowFactor: 1 } })
    inner.onmessage = value => { try { this.onmessage?.(this.frames.incoming(value)) } catch (error) { this.fail(error) } }
    inner.onclose = () => { if (!this.closed) this.fail(new Error('MCP HTTP channel closed')) }
    inner.onerror = error => { if (!this.negotiation && !this.legacyHandshake) this.fail(error) }
    return inner
  }
  private fail(error: unknown): void { if (!this.closed) this.onerror?.(error instanceof Error ? error : new Error('MCP HTTP failed')); void this.close().catch(() => undefined) }
  takeReply(operation: string): boolean { return this.frames.takeReply(operation) }
  grant(value: OperationGrant): void { if (this.closed) throw new Error('MCP transport closed'); this.frames.grant(value) }
  async start(): Promise<void> {
    if (this.started || this.closed) throw new Error('MCP HTTP already started or closed')
    this.started = true
    await this.rpc.request('net/start', { owner: this.owner, session_id: this.session, ticket: this.launch }, this.stop.signal)
    await this.inner.start()
  }
  setProtocolVersion(version: string): void { this.inner.setProtocolVersion(version) }
  async send(message: JSONRPCMessage, options?: Parameters<StreamableHTTPClientTransport['send']>[1]): Promise<void> {
    if (!this.started || this.closed || frameSize(message) > MAX_FRAME_BYTES) throw new Error('MCP HTTP write unavailable')
    const admitted = this.frames.outgoing(message), key = JSON.stringify(admitted.frame)
    if (this.writes.size >= MAX_PENDING || this.writes.has(key)) throw new Error('MCP HTTP write slot unavailable')
    this.writes.set(key, admitted)
    try {
      try { if (this.inner instanceof SSEClientTransport) await this.inner.send(admitted.frame); else await this.inner.send(admitted.frame, options) }
      catch (error) {
        const replacement = this.negotiation
        if (!replacement || this.legacy || this.closed || this.stop.signal.aborted) throw error
        this.negotiation = undefined
        const original = admitted.frame
        if (!('method' in original) || original.method !== 'initialize' || replacement.method !== original.method || replacement.operation_id !== admitted.operation_id
          || replacement.wire_id !== ('id' in original ? original.id : undefined) || !jsonEqual(replacement.params, original.params ?? {})) throw new Error('MCP negotiation grant mismatch')
        // Only Rust's explicit refusal issued this fresh, single-use grant.
        // The original numeric SDK request remains pending; peer IDs stay exact.
        this.inner.onclose = undefined; this.inner.onerror = undefined; this.inner.onmessage = undefined
        await this.inner.close()
        this.legacy = true
        this.inner = this.createInner(true)
        await this.inner.start()
        this.writes.set(key, { frame: original, ticket: replacement.ticket, operation_id: replacement.operation_id })
        await this.inner.send(original)
      }
    } catch (error) { if (!this.legacyHandshake) await this.close().catch(() => undefined); this.legacyHandshake = false; throw error }
    finally { this.writes.delete(key) }
  }
  private async fetch(input: string | URL | Request, init?: RequestInit): Promise<Response> {
    if (this.closed || this.stop.signal.aborted) throw new Error('MCP HTTP session closed')
    const url = input instanceof Request ? input.url : String(input)
    const method = init?.method ?? 'GET'
    const signal = init?.signal ? AbortSignal.any([this.stop.signal, init.signal]) : this.stop.signal
    let admitted: AdmittedFrame | undefined
    if (method === 'POST') {
      if (typeof init?.body !== 'string' || Buffer.byteLength(init.body) > MAX_FRAME_BYTES) throw new Error('MCP HTTP body unavailable')
      admitted = this.writes.get(JSON.stringify(JSON.parse(init.body)))
      if (!admitted) throw new Error('MCP HTTP body has no exact admitted frame')
      this.writes.delete(JSON.stringify(admitted.frame)) // one fetch, no SDK replay
    }
    const headers: Record<string, string> = {}
    for (const [name, value] of new Headers(init?.headers)) {
      if (!['accept', 'content-type', 'mcp-session-id', 'mcp-protocol-version', 'mcp-method', 'mcp-name'].includes(name) || value.length > 8192) throw new Error('MCP framing header unavailable')
      headers[name.replaceAll('-', '_')] = value
    }
    const reply = await this.rpc.request<{ response_id?: string; status: number; headers: Record<string, string>; legacy_grant?: OperationGrant; legacy_handshake?: boolean }>('net/fetch', { owner: this.owner, session_id: this.session, url, method, headers, ...(admitted ?? {}) }, signal)
    if (reply.legacy_handshake === true) {
      if (!admitted || !('method' in admitted.frame) || admitted.frame.method !== 'server/discover'
        || reply.response_id !== undefined || ![400, 404, 405, 406, 415].includes(reply.status)) throw new Error('MCP handshake refusal unavailable')
      this.legacyHandshake = true
    }
    if (reply.legacy_grant !== undefined) {
      if (!admitted || this.legacy || this.negotiation || reply.response_id !== undefined
        || ![400, 404, 405, 406, 415, 501].includes(reply.status)
        || typeof reply.legacy_grant.ticket !== 'string' || !reply.legacy_grant.ticket) throw new Error('MCP negotiation unavailable')
      this.negotiation = reply.legacy_grant
      throw new Error('Rust admitted legacy MCP negotiation')
    }
    if (reply.response_id === undefined) return new Response(null, { status: reply.status, headers: reply.headers })
    const response_id = reply.response_id
    let done = false
    const cancel = async () => { if (done) return; done = true; await this.rpc.request('net/release', { owner: this.owner, session_id: this.session, response_id }).catch(() => undefined) }
    const body = new ReadableStream<Uint8Array>({
      pull: async controller => { try {
        if (signal.aborted) throw new Error('MCP HTTP body cancelled')
        const next = await this.rpc.request<{ data: number[]; done: boolean }>('net/read', { owner: this.owner, session_id: this.session, response_id }, signal)
        if (!Array.isArray(next.data) || next.data.length > 32768 || next.data.some(b => !Number.isInteger(b) || b < 0 || b > 255)) throw new Error('MCP HTTP chunk exceeds bound')
        if (next.data.length) controller.enqueue(Uint8Array.from(next.data))
        if (next.done) { await cancel(); controller.close() }
      } catch (error) { await cancel(); controller.error(error); this.fail(error) } },
      cancel,
    }, { highWaterMark: 0 })
    signal.addEventListener('abort', () => { void cancel() }, { once: true })
    return new Response(body, { status: reply.status, headers: reply.headers })
  }
  async close(): Promise<void> {
    if (this.closed) return
    this.closed = true; this.stop.abort(); this.frames.clear(); this.writes.clear()
    await this.inner.close().catch(() => undefined)
    await this.rpc.request('net/close', { owner: this.owner, session_id: this.session }).catch(() => undefined)
    this.onclose?.()
  }
}
interface Session {
  client: Client
  transport: GrantedTransport
  tail: Promise<unknown>
}
export class McpModule {
  private readonly sessions = new Map<string, Session>()
  private disposed = false
  constructor(private readonly rpc: BrokerRpc, private readonly owner: OwnerRef) {}
  private check(owner: OwnerRef): void {
    if (this.disposed || identity(owner) !== identity(this.owner)) throw new Error('MCP builtin owner is no longer live')
  }
  async open(params: OpenParams, signal: AbortSignal): Promise<unknown> {
    this.check(params.owner)
    const timeout = deadline(params.deadline_ms)
    if (params.transport !== undefined && !['stdio', 'http', 'sse'].includes(params.transport)) throw new Error('MCP transport selector unavailable')
    if (this.sessions.size >= MAX_SESSIONS || this.sessions.has(params.session_id)) throw new Error('MCP session slot unavailable')
    const transport: GrantedTransport = params.transport === undefined || params.transport === 'stdio'
      ? new BrokerTransport(this.rpc, this.owner, params.session_id, params.launch_ticket)
      : new HttpBrokerTransport(this.rpc, this.owner, params.session_id, params.launch_ticket, params.transport === 'sse')
    const client = new Client({ name: 'codewhale-tui', version: params.client_version }, { capabilities: {}, supportedProtocolVersions: SUPPORTED, versionNegotiation: { mode: params.discover_grant === undefined ? 'legacy' : 'auto', probe: { timeoutMs: timeout, maxRetries: 0 } }, inputRequired: { autoFulfill: false }, listMaxPages: 64 })
    const session: Session = { client, transport, tail: Promise.resolve() }
    this.sessions.set(params.session_id, session)
    if (params.discover_grant !== undefined) transport.grant(params.discover_grant)
    transport.grant(params.initialize_grant)
    transport.grant(params.initialized_grant)
    const abort = () => { void transport.close().catch(() => undefined) }
    signal.addEventListener('abort', abort, { once: true })
    const timer = setTimeout(abort, timeout)
    timer.unref()
    try {
      if (signal.aborted) throw new Error('MCP open cancelled')
      await client.connect(transport, { signal, timeout })
      if (params.discover_grant !== undefined) transport.takeReply(params.discover_grant.operation_id)
      transport.takeReply(params.initialize_grant.operation_id)
      this.check(params.owner)
      return { protocolVersion: client.getNegotiatedProtocolVersion(), serverInfo: client.getServerVersion(), capabilities: client.getServerCapabilities(), instructions: client.getInstructions() }
    } catch (error) {
      this.sessions.delete(params.session_id)
      await transport.close().catch(() => undefined)
      throw error
    } finally { clearTimeout(timer); signal.removeEventListener('abort', abort) }
  }
  async request(params: RequestParams, signal: AbortSignal): Promise<unknown> {
    this.check(params.owner)
    const timeout = deadline(params.deadline_ms)
    const extension = EXTENSION_METHODS.has(params.grant.method)
    const method = extension ? params.grant.method : requestMethod(params.grant.method)
    const session = this.sessions.get(params.session_id)
    if (!session) throw new Error('MCP session is no longer live')
    const run = session.tail.then(async () => {
      this.check(params.owner)
      if (this.sessions.get(params.session_id) !== session || signal.aborted) throw new Error('MCP operation cancelled or stale')
      session.transport.grant(params.grant)
      // Generic request preserves per-page cursors. listTools/listResources /
      // listPrompts convenience methods would auto-aggregate into an SDK cache.
      try {
        const options = { signal, timeout, resetTimeoutOnProgress: false, allowInputRequired: true }
        return extension
          ? await session.client.request({ method, params: params.grant.params }, brokerValidated, options)
          : await session.client.request({ method: method as RequestMethod, params: params.grant.params }, options)
      } catch (error) {
        // A correlated reply completed the exchange, even if the SDK's
        // stricter result schema rejected a legacy malformed catalog item.
        // Rust receives the original bytes and applies its shared admission.
        // Every failure without that receipt retires before queued work runs.
        if (!session.transport.takeReply(params.grant.operation_id)) {
          this.sessions.delete(params.session_id)
          await session.transport.close().catch(() => undefined)
        }
        throw error
      } finally { session.transport.takeReply(params.grant.operation_id) }
    })
    session.tail = run.catch(() => undefined)
    return run
  }
  async close(owner: OwnerRef, sessionId: string): Promise<void> {
    this.check(owner)
    const session = this.sessions.get(sessionId)
    this.sessions.delete(sessionId)
    await session?.client.close()
  }
  async dispose(): Promise<void> {
    if (this.disposed) return
    this.disposed = true
    const sessions = [...this.sessions.values()]
    this.sessions.clear()
    await Promise.allSettled(sessions.map(session => session.client.close()))
  }
}
export function createMcpModule(rpc: BrokerRpc, owner: OwnerRef): McpModule {
  if (owner.plugin_id !== 'host:mcp') throw new Error('MCP protocol owner must be the pinned builtin')
  return new McpModule(rpc, Object.freeze(structuredClone(owner)))
}
