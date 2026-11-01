/** Reviewed bundle roots contribute to Rust's existing skill catalog. */
import { Service } from '@deepseek-ai/cordis'
import type { RpcPeer } from '../rpc.ts'
import { OwnedRegistrations, type OwnedEntry, type OwnerBase } from './owned.ts'

export const MAX_SKILL_ROOTS_PER_OWNER = 8
export const MAX_SKILL_ROOTS_PER_HOST = 64
export interface SkillRootDefinition { readonly path: string }
export interface LocalSkillRoot<O extends OwnerBase = OwnerBase> extends OwnedEntry<O> {}

export function normalizeSkillRoot(value: unknown): SkillRootDefinition {
  if (typeof value !== 'object' || value === null || Array.isArray(value)
    || Object.keys(value).some(key => key !== 'path')) throw new TypeError('skill root supports only path')
  const { path } = value as { path?: unknown }
  if (typeof path !== 'string' || !path || Buffer.byteLength(path, 'utf8') > 512
    || /[\\:\u0000-\u001f\u007f-\u009f]/u.test(path)
    || path.split('/').some(part => !part || part === '.' || part === '..')) {
    throw new TypeError('skill root path must be a bounded bundle-relative path with normal slash-separated components')
  }
  return Object.freeze({ path })
}

export class SkillRoots<O extends OwnerBase> {
  private readonly registrations: OwnedRegistrations<O, LocalSkillRoot<O>>
  private readonly owners = new Map<O, Map<string, () => void>>()
  private count = 0
  constructor(rpc: RpcPeer, ownedBy: (owner: O) => Map<number, LocalSkillRoot<O>>, warn: (message: string, owner: O) => void) {
    this.registrations = new OwnedRegistrations(rpc, 'skill_root', ownedBy, warn)
  }
  register(owner: O, definition: SkillRootDefinition): () => void {
    if (owner.state !== 'activating' && owner.state !== 'active') throw new Error('skill owner is not live')
    const { path } = normalizeSkillRoot(definition)
    const roots = this.owners.get(owner) ?? new Map<string, () => void>()
    if (roots.has(path)) throw new Error('skill root is already registered; dispose it before registering it again')
    if (roots.size >= MAX_SKILL_ROOTS_PER_OWNER || this.count >= MAX_SKILL_ROOTS_PER_HOST) throw new RangeError('skill root owner or host registration limit reached')
    const undo = this.registrations.add({ owner, name: path, disposed: false }, { name: path, description: '' })
    const dispose = () => {
      if (roots.get(path) !== dispose) return
      roots.delete(path)
      this.count--
      if (!roots.size) this.owners.delete(owner)
      undo()
    }
    roots.set(path, dispose)
    this.owners.set(owner, roots)
    this.count++
    return dispose
  }
  forget(owner: O) {
    for (const dispose of [...(this.owners.get(owner)?.values() ?? [])]) dispose()
    this.registrations.forget(owner)
  }
}

export function defineSkillsService<O extends OwnerBase>(host: { ownerOf(ctx: any): O | undefined, skillRoots: SkillRoots<O> }) {
  class SkillsShim extends Service {
    constructor(ctx: any) { super(ctx, 'skills') }
    registerRoot(definition: SkillRootDefinition): () => void {
      const ctx: any = this.ctx
      const owner = host.ownerOf(ctx)
      if (!owner) throw new Error('skills.registerRoot called outside an extension owner')
      const root = normalizeSkillRoot(definition)
      return ctx.effect(() => host.skillRoots.register(owner, root), `skills.registerRoot(${JSON.stringify(root.path)})`)
    }
  }
  Object.freeze(SkillsShim.prototype)
  return SkillsShim
}
