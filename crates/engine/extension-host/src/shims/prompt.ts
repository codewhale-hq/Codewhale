/** Additive prompt sections. Rust owns selection, attribution and prompt assembly. */
import { Service } from '@deepseek-ai/cordis'
import type { RpcPeer } from '../rpc.ts'
import { OwnedRegistrations, type OwnedEntry, type OwnerBase } from './owned.ts'

export const MAX_PROMPT_SECTION_BYTES = 4 * 1024
export const MAX_PROMPT_OWNER_BYTES = 32 * 1024
export const MAX_PROMPT_HOST_BYTES = 128 * 1024
export const MAX_PROMPT_SECTIONS_PER_OWNER = 128
export const MAX_PROMPT_SECTIONS_PER_HOST = 1024
const SECTION_ID = /^[a-z][a-z0-9_-]{0,63}$/u

export interface PromptSectionDefinition {
  readonly id: string
  readonly text: string
  /** Only Core supplies these values, once at the accepted turn boundary. */
  readonly interpolate?: 'model-cwd'
}

export interface LocalPromptSection<O extends OwnerBase = OwnerBase> extends OwnedEntry<O> {
  definition: PromptSectionDefinition
}

interface PromptReservation<O extends OwnerBase> {
  entry: LocalPromptSection<O>
  bytes: number
  dispose: () => void
}

/** Copy plain text now; mutating the author's object cannot change an admitted section. */
export function normalizePromptSection(value: unknown): PromptSectionDefinition {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new TypeError('prompt section must be an object with id and text')
  }
  if (Object.keys(value).some((key) => key !== 'id' && key !== 'text' && key !== 'interpolate')) {
    throw new TypeError('prompt section supports only id and text, plus optional model-cwd interpolation')
  }
  const { id, text, interpolate } = value as { id?: unknown; text?: unknown; interpolate?: unknown }
  if (typeof id !== 'string' || !SECTION_ID.test(id)) {
    throw new TypeError('prompt section id must be lower case, start with a letter, and use a-z, 0-9, _ or - (at most 64 characters)')
  }
  if (typeof text !== 'string' || text.trim().length === 0) {
    throw new TypeError(`prompt section "${id}" needs non-empty text`)
  }
  if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u.test(text)) {
    throw new TypeError(`prompt section "${id}" text contains control characters`)
  }
  if (Buffer.byteLength(text, 'utf8') > MAX_PROMPT_SECTION_BYTES) {
    throw new RangeError(`prompt section "${id}" exceeds ${MAX_PROMPT_SECTION_BYTES} UTF-8 bytes`)
  }
  if (interpolate !== undefined && interpolate !== 'model-cwd') throw new TypeError('prompt interpolation supports only model-cwd')
  if (interpolate !== undefined) validatePromptTemplate(text)
  return Object.freeze({ id, text, ...(interpolate === undefined ? {} : { interpolate }) })
}

/** Borrowed strict simple-group semantics: unmatched opens are literal; only
 * existing Core turn facts are permitted. Values are never expanded in JS. */
export function validatePromptTemplate(text: string): void {
  for (let open = text.indexOf('{{'); open >= 0;) {
    const group = /^\{\{([^{}]*)\}\}/u.exec(text.slice(open))
    if (!group) {
      if (text.indexOf('}}', open + 2) >= 0) throw new TypeError('malformed prompt variable reference')
      open = text.indexOf('{{', open + 2)
      continue
    }
    if (!/^[a-z][a-z0-9_]*$/u.test(group[1]!)) throw new TypeError('malformed prompt variable reference')
    if (group[1] !== 'model' && group[1] !== 'cwd') throw new TypeError(`unknown Core prompt variable "{{${group[1]}}}"; supported variables: model, cwd`)
    open = text.indexOf('{{', open + group[0].length)
  }
}

/** Reserve pending registrations too, so a burst cannot bypass byte/count limits. */
export class PromptSections<O extends OwnerBase> {
  private readonly registrations: OwnedRegistrations<O, LocalPromptSection<O>>
  private readonly templates: OwnedRegistrations<O, LocalPromptSection<O>>
  private readonly owners = new Map<O, Map<string, PromptReservation<O>>>()
  private bytes = 0
  private count = 0

  constructor(rpc: RpcPeer, ownedBy: (owner: O) => Map<number, LocalPromptSection<O>>, warn: (message: string, owner: O) => void) {
    this.registrations = new OwnedRegistrations(rpc, 'prompt_section', ownedBy, warn)
    this.templates = new OwnedRegistrations(rpc, 'prompt_template', ownedBy, warn)
  }

  register(owner: O, definition: PromptSectionDefinition): () => void {
    if (owner.state !== 'activating' && owner.state !== 'active') throw new Error('prompt owner is not live')
    const section = normalizePromptSection(definition)
    const sections = this.owners.get(owner) ?? new Map<string, PromptReservation<O>>()
    if (sections.has(section.id)) throw new Error(`prompt section "${section.id}" is already registered; dispose it before registering it again`)
    const bytes = Buffer.byteLength(section.text, 'utf8')
    const ownerBytes = [...sections.values()].reduce((sum, item) => sum + item.bytes, 0)
    if (ownerBytes + bytes > MAX_PROMPT_OWNER_BYTES || this.bytes + bytes > MAX_PROMPT_HOST_BYTES) {
      throw new RangeError('prompt section owner or host UTF-8 byte limit reached')
    }
    if (sections.size >= MAX_PROMPT_SECTIONS_PER_OWNER || this.count >= MAX_PROMPT_SECTIONS_PER_HOST) {
      throw new RangeError('prompt section owner or host registration limit reached')
    }
    const entry: LocalPromptSection<O> = { owner, name: section.id, definition: section, disposed: false }
    const undo = (section.interpolate === undefined ? this.registrations : this.templates).add(entry, { name: section.id, description: section.text })
    const record: PromptReservation<O> = { entry, bytes, dispose: () => {
      if (sections.get(section.id) !== record) return
      sections.delete(section.id)
      this.bytes -= bytes
      this.count -= 1
      if (sections.size === 0) this.owners.delete(owner)
      undo()
    } }
    sections.set(section.id, record)
    this.owners.set(owner, sections)
    this.bytes += bytes
    this.count += 1
    return record.dispose
  }

  /** Release reservations even after a plugin fails or times out during teardown. */
  forget(owner: O) {
    for (const record of [...(this.owners.get(owner)?.values() ?? [])]) record.dispose()
    this.registrations.forget(owner)
    this.templates.forget(owner)
  }
}

export interface PromptHost<O extends OwnerBase> {
  ownerOf(ctx: any): O | undefined
  promptSections: PromptSections<O>
}

/** The only author API: an owner-scoped contribution and its idempotent disposer. */
export function definePromptService<O extends OwnerBase>(host: PromptHost<O>) {
  class PromptShim extends Service {
    constructor(ctx: any) { super(ctx, 'prompt') }

    registerSection(definition: PromptSectionDefinition): () => void {
      const ctx: any = this.ctx
      const owner = host.ownerOf(ctx)
      if (!owner) throw new Error('prompt.registerSection called outside an extension owner')
      const section = normalizePromptSection(definition)
      return ctx.effect(() => host.promptSections.register(owner, section), `prompt.registerSection(${JSON.stringify(section.id)})`)
    }
  }
  Object.freeze(PromptShim.prototype)
  return PromptShim
}
