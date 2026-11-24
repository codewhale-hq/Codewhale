/**
 * Sandboxed Engine-brokered DSH webServer, connection, sessionTitle, and
 * profileContext adapters.
 *
 * Integration hookup (2 lines, owned by `dsh-host` / integration wave):
 * 1. In `crates/engine/extension-host/src/dsh/adapters.ts`:
 *      import { DSH_WEBSERVER_ADAPTERS } from './webserver.ts'
 *      // spread `...DSH_WEBSERVER_ADAPTERS` into `DSH_ADAPTERS`
 * 2. In `crates/engine/extension-host/src/dsh/compatibility.json`:
 *      add `"webServer"`, `"connection"`, `"sessionTitle"`, `"profileContext"` to `provided_services.host`
 */
import { EventEmitter } from 'node:events'
import { isAbsolute, resolve } from 'node:path'
import { Service } from '@deepseek-ai/cordis'
import {
  ErrorCode,
  type PluginRouteDispatchParams,
  type PluginRouteDispatchResult,
  type PluginRouteHeaderWire,
} from '../protocol.ts'
import type { HostRoot, OwnerRecord } from '../root.ts'
import { RpcError } from '../rpc.ts'
import type { DshAdapter } from './adapters.ts'

export const MAX_REQUEST_BODY_BYTES = 4 * 1024 * 1024
export const MAX_RESPONSE_BODY_BYTES = 8 * 1024 * 1024
export const MAX_ROUTE_DEADLINE_MS = 15_000
const MAX_ROUTES_PER_OWNER = 256
const MAX_SESSION_TITLES = 1024
const ALLOWED_METHODS = new Set(['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS'])

export interface NodeLikeRequest extends EventEmitter {
  method: string
  url: string
  headers: Record<string, string>
  httpVersion: string
  socket: { remoteAddress: string }
  connection: { remoteAddress: string }
  readableEnded: boolean
  destroyed: boolean
  destroy(): this
  [Symbol.asyncIterator](): AsyncGenerator<Buffer, void, unknown>
}

export interface NodeLikeResponse extends EventEmitter {
  statusCode: number
  statusMessage: string
  headersSent: boolean
  writableEnded: boolean
  finished: boolean
  setHeader(name: string, value: string | number | readonly string[]): this
  getHeader(name: string): string | string[] | undefined
  getHeaders(): Record<string, string | string[]>
  hasHeader(name: string): boolean
  removeHeader(name: string): void
  writeHead(
    statusCode: number,
    statusMessageOrHeaders?: string | Record<string, string | number | readonly string[]>,
    maybeHeaders?: Record<string, string | number | readonly string[]>,
  ): this
  flushHeaders(): void
  write(chunk: string | Uint8Array | Buffer, encoding?: BufferEncoding, cb?: () => void): boolean
  end(
    chunk?: string | Uint8Array | Buffer | (() => void),
    encoding?: BufferEncoding | (() => void),
    cb?: () => void,
  ): this
}

export type NodeRouteHandler = (req: NodeLikeRequest, res: NodeLikeResponse) => void | Promise<void>

export interface WebRouteInput {
  kind?: 'exact' | 'prefix'
  path: string
  method?: string
  methods?: readonly string[]
  handler: NodeRouteHandler
}

export interface FetchRouteInput {
  path: string
  methods?: readonly string[]
  requestBody?: string
  fetch: (request: Request) => Response | Promise<Response>
}

interface StoredRoute {
  readonly id: number
  readonly ownerToken: string
  readonly kind: 'exact' | 'prefix'
  readonly path: string
  readonly methods: ReadonlySet<string> | undefined
  readonly mode: 'node' | 'fetch' | 'rpc'
  readonly nodeHandler?: NodeRouteHandler
  readonly fetchHandler?: (request: Request) => Response | Promise<Response>
  readonly rpcHandler?: (endpoint: string, payload: unknown) => unknown | Promise<unknown>
}

let nextRouteId = 1
const routesByOwner = new Map<string, StoredRoute[]>()

function normalizePath(raw: unknown): string {
  if (typeof raw !== 'string' || raw.length === 0 || raw.length > 1024 || /[\x00-\x1f\x7f]/.test(raw)) {
    throw new RpcError(ErrorCode.InvalidParams, 'route path must be a non-empty single-line string')
  }
  const withSlash = raw.startsWith('/') ? raw : `/${raw}`
  if (withSlash.length > 1 && withSlash.endsWith('/')) {
    return withSlash.slice(0, -1)
  }
  return withSlash
}

function normalizeMethods(method?: unknown, methods?: unknown): ReadonlySet<string> | undefined {
  const list: string[] = []
  if (typeof method === 'string' && method.trim() !== '') {
    list.push(method.trim().toUpperCase())
  }
  if (Array.isArray(methods)) {
    for (const item of methods) {
      if (typeof item === 'string' && item.trim() !== '') {
        list.push(item.trim().toUpperCase())
      }
    }
  }
  return list.length > 0 ? new Set(list) : undefined
}

function addOwnerRoute(owner: OwnerRecord, route: Omit<StoredRoute, 'id' | 'ownerToken'>): () => void {
  const ownerToken = owner.ref.owner_token
  let list = routesByOwner.get(ownerToken)
  if (!list) {
    list = []
    routesByOwner.set(ownerToken, list)
  }
  if (list.length >= MAX_ROUTES_PER_OWNER) {
    throw new RpcError(ErrorCode.Refused, `owner exceeded ${MAX_ROUTES_PER_OWNER} registered routes`)
  }
  const stored: StoredRoute = { ...route, id: nextRouteId++, ownerToken }
  list.push(stored)
  let disposed = false
  return () => {
    if (disposed) return
    disposed = true
    const current = routesByOwner.get(ownerToken)
    if (!current) return
    const index = current.findIndex((entry) => entry.id === stored.id)
    if (index >= 0) current.splice(index, 1)
    if (current.length === 0) routesByOwner.delete(ownerToken)
  }
}

export function clearOwnerRoutes(ownerToken: string): void {
  routesByOwner.delete(ownerToken)
}

export function registeredRouteCount(ownerToken: string): number {
  return routesByOwner.get(ownerToken)?.length ?? 0
}

function isLoopbackHostHeader(raw: string | undefined): boolean {
  if (!raw) return true
  const trimmed = raw.trim().toLowerCase()
  if (trimmed === '') return true
  let host = trimmed
  if (host.startsWith('[')) {
    const close = host.indexOf(']')
    if (close < 0) return false
    host = host.slice(1, close)
  } else {
    const colon = host.indexOf(':')
    if (colon >= 0) host = host.slice(0, colon)
  }
  if (host === 'localhost' || host.endsWith('.localhost') || host === '::1') return true
  const match = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host)
  if (!match) return false
  const octets = match.slice(1).map(Number)
  return octets[0] === 127 && octets.every((n) => n >= 0 && n <= 255)
}

function checkRequestRejection(req: { headers?: Record<string, string | string[] | undefined> } | undefined): { status: number; message: string } | null {
  if (!req || !req.headers) return null
  const hostHeader = req.headers.host
  const host = Array.isArray(hostHeader) ? hostHeader[0] : hostHeader
  if (!isLoopbackHostHeader(host)) {
    return { status: 403, message: 'forbidden: host is not loopback' }
  }
  const fetchSiteHeader = req.headers['sec-fetch-site']
  const fetchSite = Array.isArray(fetchSiteHeader) ? fetchSiteHeader[0] : fetchSiteHeader
  if (typeof fetchSite === 'string' && fetchSite.toLowerCase() === 'cross-site') {
    return { status: 403, message: 'forbidden: cross-site request rejected' }
  }
  return null
}

function createNodeRequest(
  method: string,
  url: string,
  headers: Record<string, string>,
  body: Buffer,
): NodeLikeRequest {
  const emitter = new EventEmitter() as NodeLikeRequest
  emitter.method = method
  emitter.url = url
  emitter.headers = { host: '127.0.0.1', ...headers }
  emitter.httpVersion = '1.1'
  emitter.socket = { remoteAddress: '127.0.0.1' }
  emitter.connection = emitter.socket
  emitter.readableEnded = false
  emitter.destroyed = false
  let streamed = false

  const emitBody = () => {
    if (streamed || emitter.destroyed) return
    streamed = true
    queueMicrotask(() => {
      if (emitter.destroyed) return
      if (body.length > 0) emitter.emit('data', body)
      emitter.readableEnded = true
      emitter.emit('end')
    })
  }

  const origOn = emitter.on.bind(emitter)
  emitter.on = ((event: string | symbol, listener: (...args: any[]) => void) => {
    origOn(event, listener)
    if (event === 'data' || event === 'end') emitBody()
    return emitter
  }) as typeof emitter.on

  emitter.destroy = () => {
    emitter.destroyed = true
    return emitter
  }

  emitter[Symbol.asyncIterator] = async function* () {
    streamed = true
    if (body.length > 0 && !emitter.destroyed) {
      yield body
    }
    emitter.readableEnded = true
  }

  return emitter
}

function createNodeResponse(onFinish: () => void): {
  res: NodeLikeResponse
  result: () => { status: number; headers: PluginRouteHeaderWire[]; body: Buffer }
} {
  const emitter = new EventEmitter() as NodeLikeResponse
  const headerMap = new Map<string, string[]>()
  const chunks: Buffer[] = []
  let totalBytes = 0

  const setOneHeader = (name: string, value: string | number | readonly string[]) => {
    const key = String(name).trim().toLowerCase()
    if (!key) return
    if (Array.isArray(value)) {
      headerMap.set(key, value.map((v) => String(v)))
    } else {
      headerMap.set(key, [String(value)])
    }
  }

  const appendChunk = (chunk: string | Uint8Array | Buffer, encoding?: BufferEncoding) => {
    const buf = Buffer.isBuffer(chunk)
      ? chunk
      : typeof chunk === 'string'
        ? Buffer.from(chunk, encoding ?? 'utf8')
        : Buffer.from(chunk)
    totalBytes += buf.length
    if (totalBytes > MAX_RESPONSE_BODY_BYTES) {
      throw new RpcError(ErrorCode.ExecutionFailed, `plugin route response exceeded ${MAX_RESPONSE_BODY_BYTES} bytes`)
    }
    if (buf.length > 0) chunks.push(buf)
  }

  emitter.statusCode = 200
  emitter.statusMessage = 'OK'
  emitter.headersSent = false
  emitter.writableEnded = false
  emitter.finished = false

  emitter.setHeader = (name, value) => {
    setOneHeader(name, value)
    return emitter
  }
  emitter.getHeader = (name) => {
    const values = headerMap.get(String(name).toLowerCase())
    if (!values) return undefined
    return values.length === 1 ? values[0] : [...values]
  }
  emitter.getHeaders = () => {
    const out: Record<string, string | string[]> = {}
    for (const [k, v] of headerMap) out[k] = v.length === 1 ? v[0] : [...v]
    return out
  }
  emitter.hasHeader = (name) => headerMap.has(String(name).toLowerCase())
  emitter.removeHeader = (name) => {
    headerMap.delete(String(name).toLowerCase())
  }
  emitter.writeHead = (statusCode, statusMessageOrHeaders, maybeHeaders) => {
    emitter.statusCode = statusCode
    let headersObj: Record<string, string | number | readonly string[]> | undefined
    if (typeof statusMessageOrHeaders === 'string') {
      emitter.statusMessage = statusMessageOrHeaders
      headersObj = maybeHeaders
    } else {
      headersObj = statusMessageOrHeaders
    }
    if (headersObj && typeof headersObj === 'object') {
      for (const [k, v] of Object.entries(headersObj)) {
        if (v !== undefined) setOneHeader(k, v)
      }
    }
    emitter.headersSent = true
    return emitter
  }
  emitter.flushHeaders = () => {
    emitter.headersSent = true
  }
  emitter.write = (chunk, encoding, cb) => {
    if (emitter.writableEnded) return false
    emitter.headersSent = true
    appendChunk(chunk, encoding)
    cb?.()
    return true
  }
  emitter.end = (chunk, encoding, cb) => {
    if (emitter.writableEnded) return emitter
    let callback = cb
    if (typeof chunk === 'function') {
      callback = chunk
    } else if (chunk !== undefined) {
      const enc = typeof encoding === 'function' ? undefined : encoding
      if (typeof encoding === 'function') callback = encoding
      appendChunk(chunk, enc)
    }
    emitter.headersSent = true
    emitter.writableEnded = true
    emitter.finished = true
    callback?.()
    emitter.emit('finish')
    onFinish()
    return emitter
  }

  return {
    res: emitter,
    result: () => {
      const headers: PluginRouteHeaderWire[] = []
      for (const [name, values] of headerMap) {
        for (const value of values) {
          headers.push({ name, value })
        }
      }
      return {
        status: emitter.statusCode,
        headers,
        body: Buffer.concat(chunks, totalBytes),
      }
    },
  }
}

function matchRoute(ownerToken: string, method: string, path: string): StoredRoute | undefined {
  const routes = routesByOwner.get(ownerToken)
  if (!routes || routes.length === 0) return undefined
  const upperMethod = method.toUpperCase()
  const methodMatches = (route: StoredRoute) => !route.methods || route.methods.has(upperMethod)

  for (const route of routes) {
    if (route.kind === 'exact' && route.path === path && methodMatches(route)) {
      return route
    }
  }
  let bestPrefix: StoredRoute | undefined
  for (const route of routes) {
    if (route.kind !== 'prefix' || !methodMatches(route)) continue
    const matches =
      route.path === '/'
      || path === route.path
      || path.startsWith(`${route.path}/`)
    if (matches && (!bestPrefix || route.path.length > bestPrefix.path.length)) {
      bestPrefix = route
    }
  }
  return bestPrefix
}

async function runFetchRoute(
  route: StoredRoute,
  method: string,
  urlPath: string,
  headers: Record<string, string>,
  body: Buffer,
  signal: AbortSignal,
): Promise<PluginRouteDispatchResult> {
  const init: RequestInit = {
    method,
    headers,
    signal,
    ...(method !== 'GET' && method !== 'HEAD' && body.length > 0 ? { body: new Uint8Array(body) } : {}),
  }
  const request = new Request(`http://127.0.0.1${urlPath}`, init)
  const response = await route.fetchHandler!(request)
  const bytes = Buffer.from(await response.arrayBuffer())
  if (bytes.length > MAX_RESPONSE_BODY_BYTES) {
    throw new RpcError(ErrorCode.ExecutionFailed, `plugin route response exceeded ${MAX_RESPONSE_BODY_BYTES} bytes`)
  }
  const outHeaders: PluginRouteHeaderWire[] = []
  response.headers.forEach((value, name) => {
    outHeaders.push({ name, value })
  })
  return {
    matched: true,
    status: response.status,
    headers: outHeaders,
    ...(bytes.length > 0 ? { body_base64: bytes.toString('base64') } : {}),
  }
}

async function runRpcRoute(
  route: StoredRoute,
  path: string,
  body: Buffer,
): Promise<PluginRouteDispatchResult> {
  let parsed: any = {}
  if (body.length > 0) {
    try {
      parsed = JSON.parse(body.toString('utf8'))
    } catch {
      const errBody = Buffer.from(JSON.stringify({ error: 'invalid JSON RPC payload' }), 'utf8')
      return {
        matched: true,
        status: 400,
        headers: [{ name: 'content-type', value: 'application/json; charset=utf-8' }],
        body_base64: errBody.toString('base64'),
      }
    }
  }
  const suffix = path.length > route.path.length ? path.slice(route.path.length + 1) : ''
  const endpoint = suffix || (typeof parsed?.endpoint === 'string' ? parsed.endpoint : '')
  const payload = suffix ? parsed : (parsed?.payload ?? {})
  const result = await route.rpcHandler!(endpoint, payload)
  const outBuf = Buffer.from(JSON.stringify(result === undefined ? null : result), 'utf8')
  if (outBuf.length > MAX_RESPONSE_BODY_BYTES) {
    throw new RpcError(ErrorCode.ExecutionFailed, `plugin RPC response exceeded ${MAX_RESPONSE_BODY_BYTES} bytes`)
  }
  return {
    matched: true,
    status: 200,
    headers: [{ name: 'content-type', value: 'application/json; charset=utf-8' }],
    body_base64: outBuf.toString('base64'),
  }
}

export async function dispatchPluginRoute(
  host: Pick<HostRoot, 'owners'>,
  params: PluginRouteDispatchParams,
  signal: AbortSignal,
  runWithOwner: <T>(owner: OwnerRecord, fn: () => Promise<T>) => Promise<T> = (_owner, fn) => fn(),
): Promise<PluginRouteDispatchResult> {
  const owner = host.owners.get(params.owner.owner_token)
  if (
    !owner
    || owner.state !== 'active'
    || owner.disposing
    || owner.ref.plugin_id !== params.owner.plugin_id
    || owner.ref.generation !== params.owner.generation
  ) {
    throw new RpcError(ErrorCode.NotAvailable, `plugin owner ${params.owner.plugin_id} is not active`)
  }
  const method = params.method.toUpperCase()
  if (!ALLOWED_METHODS.has(method)) {
    throw new RpcError(ErrorCode.InvalidParams, `unsupported HTTP method ${params.method}`)
  }
  const normalizedPath = normalizePath(params.path)
  const body = params.body_base64 ? Buffer.from(params.body_base64, 'base64') : Buffer.alloc(0)
  if (body.length > MAX_REQUEST_BODY_BYTES) {
    throw new RpcError(ErrorCode.InvalidParams, `request body exceeds ${MAX_REQUEST_BODY_BYTES} bytes`)
  }
  const route = matchRoute(owner.ref.owner_token, method, normalizedPath)
  if (!route) {
    return { matched: false, status: 404, headers: [] }
  }
  const headers: Record<string, string> = {}
  for (const h of params.headers) {
    const key = h.name.trim().toLowerCase()
    if (!key) continue
    headers[key] = key in headers ? `${headers[key]}, ${h.value}` : h.value
  }
  const urlPath = params.query ? `${normalizedPath}?${params.query}` : normalizedPath
  const deadlineMs = Math.min(Math.max(params.deadline_ms || MAX_ROUTE_DEADLINE_MS, 100), MAX_ROUTE_DEADLINE_MS)
  const timeoutController = new AbortController()
  const timer = setTimeout(() => timeoutController.abort(), deadlineMs)
  timer.unref?.()
  const combinedSignal = AbortSignal.any([signal, timeoutController.signal])

  try {
    const run = runWithOwner(owner, async () => {
      if (route.mode === 'fetch') {
        return runFetchRoute(route, method, urlPath, headers, body, combinedSignal)
      }
      if (route.mode === 'rpc') {
        return runRpcRoute(route, normalizedPath, body)
      }
      let resolveDone!: () => void
      const donePromise = new Promise<void>((resolve) => {
        resolveDone = resolve
      })
      const req = createNodeRequest(method, urlPath, headers, body)
      const { res, result } = createNodeResponse(resolveDone)
      const handlerReturn = route.nodeHandler!(req, res)
      await Promise.resolve(handlerReturn)
      if (!res.writableEnded) {
        await Promise.race([
          donePromise,
          new Promise<never>((_, reject) => {
            const onAbort = () => reject(new RpcError(ErrorCode.Cancelled, 'plugin route timed out or cancelled'))
            if (combinedSignal.aborted) onAbort()
            else combinedSignal.addEventListener('abort', onAbort, { once: true })
          }),
        ])
      }
      const out = result()
      return {
        matched: true,
        status: out.status,
        headers: out.headers,
        ...(out.body.length > 0 ? { body_base64: out.body.toString('base64') } : {}),
      } satisfies PluginRouteDispatchResult
    })

    const aborted = new Promise<never>((_, reject) => {
      const onAbort = () => reject(new RpcError(ErrorCode.Cancelled, 'plugin route timed out or cancelled'))
      if (combinedSignal.aborted) onAbort()
      else combinedSignal.addEventListener('abort', onAbort, { once: true })
    })
    return await Promise.race([run, aborted])
  } finally {
    clearTimeout(timer)
  }
}

export const webServerAdapter: DshAdapter = {
  service: 'webServer',
  define(host) {
    class WebServerService extends Service {
      readonly host = '127.0.0.1'
      readonly port = 0
      readonly basePath = ''

      constructor(ctx: any) {
        super(ctx, 'webServer')
      }

      getOrigin(): string {
        return 'http://127.0.0.1'
      }

      register(route: WebRouteInput): () => void {
        const owner = host.ownerOf(this.ctx)
        if (!owner) throw new Error('webServer.register called outside an extension owner')
        if (!route || typeof route !== 'object' || typeof route.handler !== 'function') {
          throw new RpcError(ErrorCode.InvalidParams, 'webServer.register requires { path, handler }')
        }
        const kind = route.kind === 'prefix' ? 'prefix' : 'exact'
        const path = normalizePath(route.path)
        const methods = normalizeMethods(route.method, route.methods)
        return (this.ctx as any).effect(
          () => addOwnerRoute(owner, { kind, path, methods, mode: 'node', nodeHandler: route.handler }),
          `webServer.register(${kind}:${path})`,
        )
      }

      registerRoute(route: WebRouteInput): () => void {
        return this.register(route)
      }

      route(path: string, handler: NodeRouteHandler): () => void {
        return this.register({ kind: 'exact', path, handler })
      }

      get(path: string, handler: NodeRouteHandler): () => void {
        return this.register({ kind: 'exact', path, method: 'GET', handler })
      }

      post(path: string, handler: NodeRouteHandler): () => void {
        return this.register({ kind: 'exact', path, method: 'POST', handler })
      }

      put(path: string, handler: NodeRouteHandler): () => void {
        return this.register({ kind: 'exact', path, method: 'PUT', handler })
      }

      patch(path: string, handler: NodeRouteHandler): () => void {
        return this.register({ kind: 'exact', path, method: 'PATCH', handler })
      }

      delete(path: string, handler: NodeRouteHandler): () => void {
        return this.register({ kind: 'exact', path, method: 'DELETE', handler })
      }

      use(pathOrHandler: string | NodeRouteHandler, maybeHandler?: NodeRouteHandler): () => void {
        if (typeof pathOrHandler === 'function') {
          return this.register({ kind: 'prefix', path: '/', handler: pathOrHandler })
        }
        if (typeof maybeHandler !== 'function') {
          throw new RpcError(ErrorCode.InvalidParams, 'webServer.use requires a handler function')
        }
        return this.register({ kind: 'prefix', path: pathOrHandler, handler: maybeHandler })
      }

      registerUpgrade(_spec?: unknown): () => void {
        return () => {}
      }

      tapIndex(_fn?: unknown): () => void {
        return () => {}
      }
    }
    Object.freeze(WebServerService.prototype)
    return WebServerService
  },
}

export const connectionAdapter: DshAdapter = {
  service: 'connection',
  define(host) {
    class ConnectionService extends Service {
      readonly isLoopback = true
      readonly connected = true
      readonly baseUrl = 'http://127.0.0.1'

      constructor(ctx: any) {
        super(ctx, 'connection')
      }

      requestRejection(req?: { headers?: Record<string, string | string[] | undefined> }) {
        return checkRequestRejection(req)
      }

      get fetch() {
        const ctx = this.ctx
        return {
          register: (route: FetchRouteInput): () => void => {
            const owner = host.ownerOf(ctx)
            if (!owner) throw new Error('connection.fetch.register called outside an extension owner')
            if (!route || typeof route !== 'object' || typeof route.fetch !== 'function') {
              throw new RpcError(ErrorCode.InvalidParams, 'connection.fetch.register requires { path, fetch }')
            }
            const path = normalizePath(route.path)
            const methods = normalizeMethods(undefined, route.methods)
            return (ctx as any).effect(
              () => addOwnerRoute(owner, { kind: 'prefix', path, methods, mode: 'fetch', fetchHandler: route.fetch }),
              `connection.fetch.register(${path})`,
            )
          },
        }
      }

      get rpc() {
        const ctx = this.ctx
        return {
          handle: (
            channel: string,
            handler: (endpoint: string, payload: unknown) => unknown | Promise<unknown>,
            _options?: unknown,
          ): () => void => {
            const owner = host.ownerOf(ctx)
            if (!owner) throw new Error('connection.rpc.handle called outside an extension owner')
            if (typeof handler !== 'function') {
              throw new RpcError(ErrorCode.InvalidParams, 'connection.rpc.handle requires a handler function')
            }
            const path = normalizePath(channel)
            return (ctx as any).effect(
              () => addOwnerRoute(owner, { kind: 'prefix', path, methods: new Set(['POST']), mode: 'rpc', rpcHandler: handler }),
              `connection.rpc.handle(${path})`,
            )
          },
        }
      }

      onReady(cb: () => void): () => void {
        if (typeof cb === 'function') queueMicrotask(cb)
        return () => {}
      }

      onNotification(_cb: (...args: unknown[]) => void): () => void {
        return () => {}
      }
    }
    Object.freeze(ConnectionService.prototype)
    return ConnectionService
  },
}

function extractSessionId(session: unknown): string | undefined {
  if (typeof session === 'string' && session.trim() !== '') return session.trim()
  if (!session || typeof session !== 'object') return undefined
  const obj = session as Record<string, unknown>
  if (typeof obj.id === 'string' && obj.id.trim() !== '') return obj.id.trim()
  if (typeof obj.sessionId === 'string' && obj.sessionId.trim() !== '') return obj.sessionId.trim()
  const header = obj.header
  if (header && typeof header === 'object' && typeof (header as Record<string, unknown>).id === 'string') {
    const id = ((header as Record<string, unknown>).id as string).trim()
    if (id !== '') return id
  }
  return undefined
}

export const sessionTitleAdapter: DshAdapter = {
  service: 'sessionTitle',
  define() {
    const titles = new Map<string, string>()
    const listeners = new Set<(sessionId: string, title: string) => void>()

    class SessionTitleService extends Service {
      constructor(ctx: any) {
        super(ctx, 'sessionTitle')
      }

      get(session: unknown): string | undefined {
        const id = extractSessionId(session)
        return id ? titles.get(id) : undefined
      }

      rename(session: unknown, title: unknown): string {
        const id = extractSessionId(session)
        if (!id) throw new RpcError(ErrorCode.InvalidParams, 'sessionTitle.rename requires a session or session id')
        if (typeof title !== 'string') throw new RpcError(ErrorCode.InvalidParams, 'sessionTitle.rename requires a string title')
        const cleaned = title.replace(/[\r\n\t]+/g, ' ').trim().slice(0, 256)
        if (!cleaned) throw new RpcError(ErrorCode.InvalidParams, 'sessionTitle.rename requires a non-empty title')
        if (!titles.has(id) && titles.size >= MAX_SESSION_TITLES) {
          const oldest = titles.keys().next().value
          if (oldest !== undefined) titles.delete(oldest)
        }
        titles.set(id, cleaned)
        for (const listener of listeners) {
          try {
            listener(id, cleaned)
          } catch {
            // Listener failures stay isolated.
          }
        }
        return cleaned
      }

      register(listener: (sessionId: string, title: string) => void): () => void {
        if (typeof listener !== 'function') {
          throw new RpcError(ErrorCode.InvalidParams, 'sessionTitle.register requires a function')
        }
        return (this.ctx as any).effect(() => {
          listeners.add(listener)
          return () => {
            listeners.delete(listener)
          }
        }, 'sessionTitle.register')
      }
    }
    Object.freeze(SessionTitleService.prototype)
    return SessionTitleService
  },
}

export const profileContextAdapter: DshAdapter = {
  service: 'profileContext',
  define(host) {
    class ProfileContextService extends Service {
      readonly name = 'codewhale'
      readonly packageManager = 'npm'

      constructor(ctx: any) {
        super(ctx, 'profileContext')
      }

      get dir(): string {
        const owner = host.ownerOf(this.ctx)
        if (owner?.dataDir && isAbsolute(owner.dataDir)) {
          return resolve(owner.dataDir)
        }
        return resolve(process.cwd())
      }

      get installAnchor(): string {
        return this.dir
      }
    }
    Object.freeze(ProfileContextService.prototype)
    return ProfileContextService
  },
}

export const DSH_WEBSERVER_ADAPTERS: readonly DshAdapter[] = [
  webServerAdapter,
  connectionAdapter,
  sessionTitleAdapter,
  profileContextAdapter,
]
