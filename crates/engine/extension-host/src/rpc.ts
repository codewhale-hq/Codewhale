/**
 * JSON-RPC 2.0 peer over the `CWX1` channel.
 *
 * Every inbound request gets an `AbortController`; `$/cancel {id}` from the
 * core aborts it. The core resolves a cancelled call on its own side after a
 * 500 ms grace, so a late answer here is harmless (the core drops it).
 */
import { ErrorCode, MAX_INFLIGHT, validateMessage, type HostTier, type Message, type RpcErrorWire } from './protocol.ts'

export class RpcError extends Error {
  constructor(
    readonly code: number,
    message: string,
    readonly data?: unknown,
  ) {
    super(message)
    this.name = 'RpcError'
  }

  toWire(): RpcErrorWire {
    return this.data === undefined
      ? { code: this.code, message: this.message }
      : { code: this.code, message: this.message, data: this.data as never }
  }
}

export interface RequestContext {
  id: number
  signal: AbortSignal
}

type RequestHandler = (params: any, cx: RequestContext) => Promise<unknown> | unknown
type NotificationHandler = (params: any) => void

export class RpcPeer {
  private nextId = 1
  private readonly pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>()
  private readonly inbound = new Map<number, AbortController>()
  private readonly requestHandlers = new Map<string, RequestHandler>()
  private readonly notificationHandlers = new Map<string, NotificationHandler>()
  private closed = false

  constructor(
    private readonly send: (message: Message) => void,
    /** The trust tier this host serves: methods reserved for the other tier are neither sent nor accepted. */
    private readonly tier: HostTier,
  ) {}

  onRequest(method: string, handler: RequestHandler) {
    this.requestHandlers.set(method, handler)
  }

  onNotification(method: string, handler: NotificationHandler) {
    this.notificationHandlers.set(method, handler)
  }

  /**
   * Send a host→core request. Outbound messages are validated strictly first.
   * When `signal` aborts before the answer, the core is sent `$/cancel` for it
   * and the promise rejects as cancelled at once; an answer that still arrives
   * is dropped (the core drops its own after a cancel as well).
   */
  request<T = any>(method: string, params: unknown, signal?: AbortSignal): Promise<T> {
    if (this.closed) return Promise.reject(new RpcError(ErrorCode.NotAvailable, 'channel closed'))
    if (signal?.aborted) return Promise.reject(new RpcError(ErrorCode.Cancelled, 'cancelled'))
    // The core's per-direction limit.
    if (this.pending.size >= MAX_INFLIGHT) {
      return Promise.reject(new RpcError(ErrorCode.Internal, `more than ${MAX_INFLIGHT} requests in flight`))
    }
    const id = this.nextId++
    const message = { jsonrpc: '2.0' as const, id, method, params }
    validateMessage(message, 'host_to_core', this.tier)
    return new Promise<T>((resolve, reject) => {
      const onAbort = () => {
        if (!this.pending.delete(id)) return
        this.notify('$/cancel', { id })
        reject(new RpcError(ErrorCode.Cancelled, 'cancelled'))
      }
      this.pending.set(id, {
        resolve: (value) => {
          signal?.removeEventListener('abort', onAbort)
          resolve(value)
        },
        reject: (error) => {
          signal?.removeEventListener('abort', onAbort)
          reject(error)
        },
      })
      signal?.addEventListener('abort', onAbort, { once: true })
      this.send(message)
    })
  }

  notify(method: string, params: unknown) {
    if (this.closed) return
    const message = { jsonrpc: '2.0' as const, method, params }
    validateMessage(message, 'host_to_core', this.tier)
    this.send(message)
  }

  /** Dispatch one decoded core→host message. Throws `ProtocolError` for malformed input. */
  handle(raw: unknown) {
    const message = validateMessage(raw, 'core_to_host', this.tier) as any
    if ('method' in message) {
      if (message.method === '$/cancel') {
        this.inbound.get(message.params.id)?.abort()
        return
      }
      if ('id' in message) {
        void this.dispatchRequest(message.id, message.method, message.params ?? {})
        return
      }
      this.notificationHandlers.get(message.method)?.(message.params ?? {})
      return
    }
    const waiter = this.pending.get(message.id)
    if (!waiter) return
    this.pending.delete(message.id)
    if ('error' in message) {
      waiter.reject(new RpcError(message.error.code, message.error.message, message.error.data))
    } else {
      waiter.resolve(message.result)
    }
  }

  /** Fail every outbound request; used when the channel closes. */
  close(reason: string) {
    this.closed = true
    for (const [, waiter] of this.pending) waiter.reject(new RpcError(ErrorCode.NotAvailable, reason))
    this.pending.clear()
    for (const [, controller] of this.inbound) controller.abort()
  }

  private async dispatchRequest(id: number, method: string, params: unknown) {
    const handler = this.requestHandlers.get(method)
    if (!handler) {
      this.reply(id, undefined, new RpcError(ErrorCode.MethodNotFound, `no handler for \`${method}\``))
      return
    }
    const controller = new AbortController()
    this.inbound.set(id, controller)
    try {
      const result = await handler(params, { id, signal: controller.signal })
      this.reply(id, result ?? {})
    } catch (error) {
      this.reply(id, undefined, toRpcError(error, controller.signal))
    } finally {
      this.inbound.delete(id)
    }
  }

  private reply(id: number, result: unknown, error?: RpcError) {
    if (this.closed) return
    this.send(error ? { jsonrpc: '2.0', id, error: error.toWire() } : { jsonrpc: '2.0', id, result })
  }
}

export function toRpcError(error: unknown, signal?: AbortSignal): RpcError {
  if (error instanceof RpcError) return error
  if (signal?.aborted) return new RpcError(ErrorCode.Cancelled, 'cancelled')
  const message = error instanceof Error ? `${error.name}: ${error.message}` : String(error)
  return new RpcError(ErrorCode.ExecutionFailed, message.slice(0, 4096))
}
