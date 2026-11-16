import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import type { AdapterHost, DshAdapter } from './adapters.ts'

export interface SettingsDescriptor {
  ns: string
  writable: boolean
  roots: string[]
  value: Record<string, unknown>
  revision: string
  applied: boolean | null
  applies: 'reload'
}

export type SettingsPathOp =
  | { op: 'set', path: readonly string[], value: unknown }
  | { op: 'unset', path: readonly string[] }

export interface SettingsWrite {
  written: boolean
  revision: string
  applies: 'reload'
}

export class SettingsConflictError extends Error {
  readonly code = 'SETTINGS_CONFLICT'
  constructor(readonly ns: string, readonly expected: string | undefined, readonly actual: string) {
    super(`settings namespace "${ns}" changed since it was read`)
    this.name = 'SettingsConflictError'
  }
}

function plain(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new RpcError(ErrorCode.InvalidParams, `${what} is a plain object`)
  return value as Record<string, unknown>
}

function revision(value: unknown): string | undefined {
  if (value === undefined) return undefined
  if (typeof value !== 'string' || value.length === 0) throw new RpcError(ErrorCode.InvalidParams, 'an expected revision is the revision string describe returned')
  return value
}

function descriptor(value: unknown): SettingsDescriptor {
  const row = value as Partial<SettingsDescriptor> | null
  if (row === null || typeof row !== 'object' || typeof row.ns !== 'string' || typeof row.revision !== 'string' || typeof row.writable !== 'boolean'
    || !Array.isArray(row.roots) || typeof row.value !== 'object' || row.value === null || row.applies !== 'reload') {
    throw new RpcError(ErrorCode.Internal, 'invalid settings broker response')
  }
  return { ns: row.ns, writable: row.writable, roots: row.roots.map(String), value: row.value, revision: row.revision, applied: typeof row.applied === 'boolean' ? row.applied : null, applies: 'reload' }
}

export const settingsAdapter: DshAdapter = {
  service: 'settings',
  define(host: AdapterHost) {
    class SettingsService extends Service {
      constructor(ctx: any) { super(ctx, 'settings') }

      async describe(): Promise<SettingsDescriptor[]> {
        return [descriptor(await host.broker(this.ctx, 'settings', 'describe', {}))]
      }

      async update(ns: unknown, patch: unknown, expectedRevision?: unknown): Promise<SettingsWrite> {
        return this.write(ns, 'update', { patch: plain(patch, 'a settings patch') }, expectedRevision)
      }

      async replace(ns: unknown, section: unknown, expectedRevision?: unknown): Promise<SettingsWrite> {
        return this.write(ns, 'replace', { value: plain(section, 'a settings section') }, expectedRevision)
      }

      async mutate(ns: unknown, ops: unknown, expectedRevision?: unknown): Promise<SettingsWrite> {
        if (!Array.isArray(ops)) throw new RpcError(ErrorCode.InvalidParams, 'settings edits are an array of path operations')
        return this.write(ns, 'mutate', { ops }, expectedRevision)
      }

      private async write(ns: unknown, method: 'update' | 'replace' | 'mutate', input: Record<string, unknown>, expectedRevision: unknown): Promise<SettingsWrite> {
        if (typeof ns !== 'string' || ns.length === 0) throw new RpcError(ErrorCode.InvalidParams, 'a settings namespace is a non-empty string')
        const owner = host.ownerOf(this.ctx)
        if (owner === undefined || ns !== owner.pluginName) throw new RpcError(ErrorCode.Refused, 'a plugin may edit only its own settings namespace')
        const expected = revision(expectedRevision)
        const result = await host.broker(this.ctx, 'settings', method, expected === undefined ? input : { ...input, expected_revision: expected }) as { written?: unknown, conflict?: unknown, revision?: unknown } | null
        if (result === null || typeof result !== 'object' || typeof result.written !== 'boolean' || typeof result.revision !== 'string') throw new RpcError(ErrorCode.Internal, 'invalid settings broker response')
        if (result.conflict === true) throw new SettingsConflictError(ns, expected, result.revision)
        return { written: result.written, revision: result.revision, applies: 'reload' }
      }
    }
    Object.freeze(SettingsService.prototype)
    return SettingsService
  },
}
