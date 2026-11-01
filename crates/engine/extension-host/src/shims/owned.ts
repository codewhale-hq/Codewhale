/**
 * Owned registrations: the host half of `registry/register` for one kind.
 *
 * Every shim that contributes something to the core (tools, commands) follows
 * the same life cycle: propose it with `registry/register`, keep the handle
 * the core admitted, undo exactly that handle when the plugin's effect is
 * disposed, and report a refusal so activation fails with the core's reason.
 * The Rust side is the authority (`OwnerRegistry`); this only keeps the
 * host's own index by handle so a later `tool/call` or `command/run` can find
 * the definition.
 */
import type { OwnerRef, EntryRef } from '../protocol.ts'
import type { RpcPeer } from '../rpc.ts'

/** What an owner record must carry for registrations to be tracked on it. */
export interface OwnerBase {
  ref: OwnerRef
  scope?: EntryRef
  state: 'activating' | 'active' | 'failed' | 'disposed'
  /** In-flight `registry/register` requests, awaited before activation acks. */
  pendingRegistrations: Set<Promise<void>>
  refusals: string[]
}

/** One registration a plugin made, from this host's side. */
export interface OwnedEntry<O extends OwnerBase> {
  owner: O
  name: string
  /** Set when the core admits it. */
  handle?: number
  disposed: boolean
}

export type RegisterSpec = Record<string, unknown>

function describeError(error: unknown): string {
  return error instanceof Error ? `${error.name}: ${error.message}` : String(error)
}

export class OwnedRegistrations<O extends OwnerBase, T extends OwnedEntry<O>> {
  /** Admitted entries by core handle. */
  readonly byHandle = new Map<number, T>()

  private readonly rpc: RpcPeer
  private readonly kind: 'tool' | 'command' | 'hook' | 'prompt_section' | 'prompt_template' | 'skill_root' | 'avatar_pack' | 'shell_hook' | 'mcp_server'
  /** The owner's own index of this kind, for the leak report at deactivation. */
  private readonly ownedBy: (owner: O) => Map<number, T>
  private readonly warn: (message: string, owner: O) => void

  constructor(
    rpc: RpcPeer,
    kind: 'tool' | 'command' | 'hook' | 'prompt_section' | 'prompt_template' | 'skill_root' | 'avatar_pack' | 'shell_hook' | 'mcp_server',
    ownedBy: (owner: O) => Map<number, T>,
    warn: (message: string, owner: O) => void,
  ) {
    this.rpc = rpc
    this.kind = kind
    this.ownedBy = ownedBy
    this.warn = warn
  }

  /** Called inside the owner's effect; returns the effect's cleanup. */
  add(entry: T, spec: RegisterSpec): () => void {
    const owner = entry.owner
    if (owner.state!=='activating' && owner.state!=='active') throw new Error('registration owner was disposed')
    const registration: Promise<void> = this.rpc
      .request('registry/register', { owner: owner.ref, ...(owner.scope ? {scope:owner.scope} : {}), kind: this.kind, spec })
      .then(
        (result: any) => {
          if (typeof result?.handle === 'number') {
            entry.handle = result.handle
            if (entry.disposed || (owner.state!=='activating' && owner.state!=='active')) {
              return this.unregister(entry)
            }
            this.ownedBy(owner).set(result.handle, entry)
            this.byHandle.set(result.handle, entry)
          } else {
            const reason = typeof result?.refused === 'string' ? result.refused : 'refused without a reason'
            owner.refusals.push(`${this.kind} \`${entry.name}\` refused: ${reason}`)
            if (owner.state === 'active') this.warn(`${this.kind} \`${entry.name}\` refused: ${reason}`, owner)
          }
        },
        (error: unknown) => {
          owner.refusals.push(`${this.kind} \`${entry.name}\` registration failed: ${describeError(error)}`)
        },
      )
      .finally(() => owner.pendingRegistrations.delete(registration))
    owner.pendingRegistrations.add(registration)
    return () => {
      if (entry.disposed) return
      entry.disposed = true
      if (entry.handle !== undefined) void this.unregister(entry)
    }
  }

  /** Undo exactly this entry. The core revokes first, so a failure here is expected after revocation. */
  private unregister(entry: T): Promise<void> {
    const handle = entry.handle!
    const owner = entry.owner
    this.ownedBy(owner).delete(handle)
    this.byHandle.delete(handle)
    // Rollback is part of owner quiescence, including an admission response
    // arriving after its fiber failed. No activation/disposal ack precedes it.
    const cleanup: Promise<void> = this.rpc
      .request('registry/unregister', { owner: owner.ref, handle })
      .then(() => undefined, () => undefined) // Core may have revoked first.
      .finally(() => owner.pendingRegistrations.delete(cleanup))
    owner.pendingRegistrations.add(cleanup)
    return cleanup
  }

  /** Drop the host's index of everything `owner` registered (after its teardown). */
  forget(owner: O) {
    for (const entry of this.ownedBy(owner).values()) this.byHandle.delete(entry.handle!)
  }
}
