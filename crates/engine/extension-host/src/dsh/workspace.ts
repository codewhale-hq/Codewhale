import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import type { DshAdapter } from './adapters.ts'

const MAX_PATH = 512
const MAX_WRITE = 1024 * 1024
const MAX_ARGV = 9
const MAX_ARG = 1024

function invalid(message: string): never {
  throw new RpcError(ErrorCode.InvalidParams, message)
}

function path(value: unknown): string {
  if (typeof value !== 'string' || value.length === 0 || value.length > MAX_PATH || value.trim() !== value || /[\x00-\x1f\x7f]/.test(value)) {
    invalid(`a path is 1 to ${MAX_PATH} characters on one line with no surrounding spaces`)
  }
  return value as string
}

function bounded(value: unknown, what: string, min: number, max: number): number | undefined {
  if (value === undefined) return undefined
  if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > max) invalid(`${what} is an integer between ${min} and ${max}`)
  return value as number
}

function argv(value: unknown): string[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_ARGV || value.some(arg => typeof arg !== 'string' || arg.length > MAX_ARG || /[\x00-\x1f\x7f]/.test(arg))) {
    invalid(`argv is 1 to ${MAX_ARGV} strings of at most ${MAX_ARG} characters`)
  }
  return [...value as string[]]
}

function compact(input: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(input).filter(([, value]) => value !== undefined))
}

export const workspaceAdapter: DshAdapter = {
  service: 'workspace',
  define(host) {
    class WorkspaceService extends Service {
      constructor(ctx: any) { super(ctx, 'workspace') }
      read(file: unknown, options?: { startLine?: unknown, maxLines?: unknown }) {
        return host.broker(this.ctx, 'workspace', 'read', compact({
          path: path(file),
          start_line: bounded(options?.startLine, 'startLine', 1, Number.MAX_SAFE_INTEGER),
          max_lines: bounded(options?.maxLines, 'maxLines', 1, 10_000),
        }))
      }
      list(dir?: unknown) {
        return host.broker(this.ctx, 'workspace', 'list', compact({ path: dir === undefined ? undefined : path(dir) }))
      }
      write(file: unknown, content: unknown, options?: { expectedHash?: unknown }) {
        if (typeof content !== 'string' || Buffer.byteLength(content) > MAX_WRITE) invalid(`content is a string of at most ${MAX_WRITE} bytes`)
        const hash = options?.expectedHash
        if (hash !== undefined && (typeof hash !== 'string' || hash.length === 0 || hash.length > 128)) invalid('expectedHash is 1 to 128 characters')
        return host.broker(this.ctx, 'workspace', 'write', compact({ path: path(file), content, expected_hash: hash }))
      }
      exec(command: unknown, options?: { cwd?: unknown }) {
        return host.broker(this.ctx, 'workspace', 'exec', compact({
          argv: argv(command),
          cwd: options?.cwd === undefined ? undefined : path(options.cwd),
        }))
      }
    }
    Object.freeze(WorkspaceService.prototype)
    return WorkspaceService
  },
}
