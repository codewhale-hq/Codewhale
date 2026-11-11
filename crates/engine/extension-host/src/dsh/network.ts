import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import type { AdapterHost, DshAdapter } from './adapters.ts'

export const CREDENTIAL_HANDLE_PREFIX = 'codewhale-credential:'

interface BrokeredResponse {
  status: number
  url: string
  redirected: boolean
  headers: [string, string][]
  body_base64: string
}

function credentialRef(ref: unknown): string {
  if (typeof ref !== 'string' || ref.length === 0) throw new RpcError(ErrorCode.InvalidParams, 'a credential ref is a non-empty string')
  return ref
}

function brokered(value: unknown): BrokeredResponse {
  const response = value as Partial<BrokeredResponse> | null
  if (response === null || typeof response !== 'object' || typeof response.status !== 'number' || typeof response.url !== 'string'
    || typeof response.body_base64 !== 'string' || !Array.isArray(response.headers)) {
    throw new RpcError(ErrorCode.Internal, 'invalid network broker response')
  }
  return response as BrokeredResponse
}

async function requestBody(body: unknown): Promise<string | undefined> {
  if (body === undefined || body === null) return undefined
  if (typeof body === 'string') return Buffer.from(body, 'utf8').toString('base64')
  if (body instanceof ArrayBuffer) return Buffer.from(body).toString('base64')
  if (ArrayBuffer.isView(body)) return Buffer.from(body.buffer, body.byteOffset, body.byteLength).toString('base64')
  if (body instanceof URLSearchParams) return Buffer.from(body.toString(), 'utf8').toString('base64')
  if (body instanceof Blob) return Buffer.from(await body.arrayBuffer()).toString('base64')
  throw new RpcError(ErrorCode.InvalidParams, 'the network broker sends string, binary, URLSearchParams or Blob bodies')
}

export async function brokeredFetch(host: AdapterHost, ctx: unknown, input: string | URL | Request, init: RequestInit = {}): Promise<Response> {
  const request = input instanceof Request ? input : undefined
  const url = request?.url ?? String(input)
  const headers = new Headers(request?.headers)
  new Headers(init.headers).forEach((value, name) => headers.set(name, value))
  const body = init.body !== undefined ? init.body : request !== undefined && request.body !== null ? await request.arrayBuffer() : undefined
  const result = brokered(await host.broker(ctx, 'network', 'fetch', {
    url,
    method: init.method ?? request?.method ?? 'GET',
    headers: [...headers.entries()],
    body_base64: await requestBody(body),
  }))
  const bytes = Buffer.from(result.body_base64, 'base64')
  const nullBody = [101, 204, 205, 304].includes(result.status)
  const response = new Response(nullBody ? null : bytes, { status: result.status, headers: result.headers })
  Object.defineProperty(response, 'url', { value: result.url })
  Object.defineProperty(response, 'redirected', { value: result.redirected === true })
  return response
}

export const credentialsAdapter: DshAdapter = {
  service: 'credentials',
  define(host) {
    class CredentialsService extends Service {
      constructor(ctx: any) { super(ctx, 'credentials') }

      async resolve(ref: unknown): Promise<{ value: string } | undefined> {
        const result = await host.broker(this.ctx, 'network', 'credentials.resolve', { ref: credentialRef(ref) }) as { value?: unknown } | null
        const value = result?.value
        if (value === null || value === undefined) return undefined
        if (typeof value !== 'string' || !value.startsWith(CREDENTIAL_HANDLE_PREFIX)) throw new RpcError(ErrorCode.Internal, 'invalid credential handle')
        return { value }
      }

      async describe(ref: unknown): Promise<{ ref: string, set: boolean }> {
        const result = await host.broker(this.ctx, 'network', 'credentials.describe', { ref: credentialRef(ref) }) as { ref?: unknown, set?: unknown } | null
        if (result === null || typeof result !== 'object' || typeof result.set !== 'boolean') throw new RpcError(ErrorCode.Internal, 'invalid credential description')
        return { ref: credentialRef(ref), set: result.set }
      }

      async set(ref: unknown, value: unknown): Promise<void> {
        if (typeof value !== 'string' || value.length === 0) throw new RpcError(ErrorCode.InvalidParams, 'a credential value is a non-empty string')
        await host.broker(this.ctx, 'network', 'credentials.set', { ref: credentialRef(ref), value })
      }

      fetch(input: string | URL | Request, init?: RequestInit): Promise<Response> {
        return brokeredFetch(host, this.ctx, input, init)
      }
    }
    Object.freeze(CredentialsService.prototype)
    return CredentialsService
  },
}
