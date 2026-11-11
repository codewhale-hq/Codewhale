import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import type { DshAdapter } from './adapters.ts'

const SESSION = /^[A-Za-z0-9._-]{1,64}$/

function invalid(message: string): never {
  throw new RpcError(ErrorCode.InvalidParams, message)
}

function session(value: unknown): string {
  if (typeof value !== 'string' || !SESSION.test(value)) invalid('a terminal name is 1 to 64 letters, digits, `_`, `-` or `.`')
  return value as string
}

function text(value: unknown, what: string): string {
  if (typeof value !== 'string' || value.length === 0) invalid(`${what} is a non-empty string`)
  return value as string
}

function bounded(value: unknown, what: string, min: number, max: number): number | undefined {
  if (value === undefined) return undefined
  if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > max) invalid(`${what} is an integer between ${min} and ${max}`)
  return value as number
}

function compact(input: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(input).filter(([, value]) => value !== undefined))
}

export const terminalAdapter: DshAdapter = {
  service: 'terminal',
  define(host) {
    class TerminalService extends Service {
      constructor(ctx: any) { super(ctx, 'terminal') }
      list() { return host.broker(this.ctx, 'terminal', 'list', {}) }
      run(command: unknown, options?: { session?: unknown, timeoutSecs?: unknown }) {
        return host.broker(this.ctx, 'terminal', 'run', compact({
          command: text(command, 'a command'),
          session: options?.session === undefined ? undefined : session(options.session),
          timeout_secs: bounded(options?.timeoutSecs, 'timeoutSecs', 1, 600),
        }))
      }
      send(name: unknown, input: unknown, options?: { waitMs?: unknown }) {
        return host.broker(this.ctx, 'terminal', 'send', compact({
          session: session(name),
          text: text(input, 'terminal input'),
          wait_ms: bounded(options?.waitMs, 'waitMs', 0, 60_000),
        }))
      }
      wait(name: unknown, options?: { timeoutSecs?: unknown }) {
        return host.broker(this.ctx, 'terminal', 'wait', compact({
          session: session(name),
          timeout_secs: bounded(options?.timeoutSecs, 'timeoutSecs', 1, 600),
        }))
      }
      cancel(name: unknown) { return host.broker(this.ctx, 'terminal', 'cancel', { session: session(name) }) }
      reset(name: unknown) { return host.broker(this.ctx, 'terminal', 'reset', { session: session(name) }) }
    }
    Object.freeze(TerminalService.prototype)
    return TerminalService
  },
}
