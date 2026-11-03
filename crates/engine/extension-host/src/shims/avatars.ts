/** Reviewed bundle roots contribute to Rust's existing avatar catalog. */
import { Service } from '@deepseek-ai/cordis'
import type { RpcPeer } from '../rpc.ts'
import { OwnedRegistrations, type OwnedEntry, type OwnerBase } from './owned.ts'

export const MAX_AVATAR_PACKS_PER_OWNER = 4
export const MAX_AVATAR_PACKS_PER_HOST = 16
export interface AvatarPackDefinition { readonly path: string }
export interface LocalAvatarPack<O extends OwnerBase = OwnerBase> extends OwnedEntry<O> {}

export function normalizeAvatarPack(value: unknown): AvatarPackDefinition {
  if (typeof value !== 'object' || value === null || Array.isArray(value)
    || Object.keys(value).some(key => key !== 'path')) throw new TypeError('avatar pack supports only path')
  const { path } = value as { path?: unknown }
  if (typeof path !== 'string' || !path || Buffer.byteLength(path, 'utf8') > 256 || !path.endsWith('.json')
    || /[\\:\u0000-\u001f\u007f-\u009f]/u.test(path)
    || path.split('/').some(part => !part || part === '.' || part === '..')) {
    throw new TypeError('avatar pack path must be a bounded bundle-relative path with normal slash-separated components')
  }
  return Object.freeze({ path })
}

export class AvatarPacks<O extends OwnerBase> {
  private readonly registrations: OwnedRegistrations<O, LocalAvatarPack<O>>
  private readonly owners = new Map<O, Map<string, () => void>>()
  private count = 0
  constructor(rpc: RpcPeer, ownedBy: (owner: O) => Map<number, LocalAvatarPack<O>>, warn: (message: string, owner: O) => void) {
    this.registrations = new OwnedRegistrations(rpc, 'avatar_pack', ownedBy, warn)
  }
  register(owner: O, definition: AvatarPackDefinition): () => void {
    if (owner.state !== 'activating' && owner.state !== 'active') throw new Error('avatar owner is not live')
    const { path } = normalizeAvatarPack(definition)
    const roots = this.owners.get(owner) ?? new Map<string, () => void>()
    if (roots.has(path)) throw new Error('avatar pack is already registered; dispose it before registering it again')
    if (roots.size >= MAX_AVATAR_PACKS_PER_OWNER || this.count >= MAX_AVATAR_PACKS_PER_HOST) throw new RangeError('avatar pack owner or host registration limit reached')
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

export function defineAvatarsService<O extends OwnerBase>(host: { ownerOf(ctx: any): O | undefined, avatarPacks: AvatarPacks<O> }) {
  class AvatarsShim extends Service {
    constructor(ctx: any) { super(ctx, 'avatars') }
    registerPack(definition: AvatarPackDefinition): () => void {
      const ctx: any = this.ctx
      const owner = host.ownerOf(ctx)
      if (!owner) throw new Error('avatars.registerPack called outside an extension owner')
      const root = normalizeAvatarPack(definition)
      return ctx.effect(() => host.avatarPacks.register(owner, root), `avatars.registerPack(${JSON.stringify(root.path)})`)
    }
  }
  Object.freeze(AvatarsShim.prototype)
  return AvatarsShim
}
