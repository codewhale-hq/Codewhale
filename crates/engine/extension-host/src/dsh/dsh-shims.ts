import { Service } from '@deepseek-ai/cordis'
import { randomBytes } from 'node:crypto'
import { lstat, mkdir, rename, rm, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute, join, resolve } from 'node:path'
import { ErrorCode } from '../protocol.ts'
import { ownerStorage } from '../root.ts'
import { RpcError } from '../rpc.ts'
import { canonicalPath, insideKey, unlinkedKeyMatches } from './canonical-path.ts'
import compatibility from './compatibility.json' with { type: 'json' }
import { SettingsConflictError } from './settings.ts'
import { HarnessError } from './dsh-message-compat.js'

const MAX_CREDENTIAL_REF_LEN = 256
const MAX_ATOMIC_WRITE_BYTES = 16 * 1024 * 1024

function activeDataDir(operation: string): string {
  const owner = ownerStorage.getStore()
  if (!owner || typeof owner.dataDir !== 'string' || owner.dataDir.length === 0) {
    throw new Error(`${operation} requires an active plugin owner with a scoped dataDir`)
  }
  return canonicalPath(owner.dataDir)
}

function assertNoEscapingAncestor(dataRoot: string, target: string, operation: string): void {
  let current = target
  while (current.length >= dataRoot.length) {
    if (current === dataRoot) break
    try {
      const canonical = canonicalPath(current)
      const key = insideKey(dataRoot, current)
      if (key === undefined || key === '') {
        if (canonical !== dataRoot) {
          throw new Error(`${operation} refused symlink escaping plugin dataDir`)
        }
        break
      }
      if (!unlinkedKeyMatches(dataRoot, key, canonical)) {
        throw new Error(`${operation} refused symlink escaping plugin dataDir`)
      }
      break
    } catch (error: any) {
      if (error?.code === 'ENOENT') {
        const next = dirname(current)
        if (next === current) break
        current = next
        continue
      }
      throw error
    }
  }
}

function confinePathToDataDir(targetPath: unknown, operation: string): string {
  if (typeof targetPath !== 'string' || targetPath.length === 0 || /[\x00-\x1f\x7f]/.test(targetPath)) {
    throw new TypeError(`${operation} requires a valid file path string`)
  }
  const dataRoot = activeDataDir(operation)
  const resolved = isAbsolute(targetPath) ? resolve(targetPath) : resolve(dataRoot, targetPath)
  const rel = insideKey(dataRoot, resolved)
  if (rel === undefined || rel === '') {
    throw new Error(`${operation} refused path outside plugin dataDir`)
  }
  assertNoEscapingAncestor(dataRoot, dirname(resolved), operation)
  return resolved
}

// ---------------------------------------------------------------------------
// @deepseek-ai/dsh-credentials (plugin-scoped handle helpers; never host secrets)
// ---------------------------------------------------------------------------
export function credentialRef(value: unknown): string {
  if (typeof value !== 'string') {
    throw new TypeError('credentialRef requires a non-empty string')
  }
  const trimmed = value.trim()
  if (trimmed.length === 0 || trimmed.length > MAX_CREDENTIAL_REF_LEN || /[\x00-\x1f\x7f]/.test(trimmed)) {
    throw new TypeError('credentialRef requires a bounded printable string')
  }
  return trimmed
}

export async function resolveCredential(
  ctx: { credentials?: { resolve(ref: string): Promise<{ value: string } | undefined> } },
  ref: string,
): Promise<{ value: string } | undefined> {
  if (!ctx?.credentials || typeof ctx.credentials.resolve !== 'function') return undefined
  return ctx.credentials.resolve(credentialRef(ref))
}

export async function describeCredential(
  ctx: { credentials?: { describe(ref: string): Promise<{ ref: string, set: boolean, site?: string }> } },
  ref: string,
): Promise<{ ref: string, set: boolean, site?: string } | undefined> {
  if (!ctx?.credentials || typeof ctx.credentials.describe !== 'function') return undefined
  return ctx.credentials.describe(credentialRef(ref))
}

export const dshCredentialsModule = Object.freeze({
  credentialRef,
  resolveCredential,
  describeCredential,
})

// ---------------------------------------------------------------------------
// @deepseek-ai/dsh-settings (backed by ctx.settings)
// ---------------------------------------------------------------------------
export function settingsNamespace(name: unknown): string {
  if (typeof name !== 'string' || name.trim().length === 0 || name.length > 128) {
    throw new TypeError('settingsNamespace requires a non-empty string')
  }
  return name.trim()
}

export function installSettingsSection(
  ctx: { settings?: { describe(): unknown } },
  ns: string,
  _section?: unknown,
): () => void {
  settingsNamespace(ns)
  void ctx?.settings
  return () => undefined
}

export const dshSettingsModule = Object.freeze({
  SettingsConflictError,
  settingsNamespace,
  installSettingsSection,
  default: Object.freeze({
    SettingsConflictError,
    settingsNamespace,
    installSettingsSection,
  }),
})

// ---------------------------------------------------------------------------
// @deepseek-ai/dsh-home-paths (strictly confined inside owner.dataDir)
// ---------------------------------------------------------------------------
export function resolveDshHome(...segments: string[]): string {
  const dataRoot = activeDataDir('resolveDshHome')
  if (segments.length === 0) return dataRoot
  for (const seg of segments) {
    if (typeof seg !== 'string' || seg.includes('\0') || isAbsolute(seg)) {
      throw new Error('resolveDshHome refused absolute or invalid path segment')
    }
  }
  const target = resolve(dataRoot, ...segments)
  const rel = insideKey(dataRoot, target)
  if (rel === undefined) {
    throw new Error('resolveDshHome refused path escaping plugin dataDir')
  }
  if (rel !== '') {
    assertNoEscapingAncestor(dataRoot, target, 'resolveDshHome')
  }
  return target
}

export const dshHomePathsModule = Object.freeze({
  resolveDshHome,
  resolveDshPath: resolveDshHome,
})

// ---------------------------------------------------------------------------
// @deepseek-ai/dsh-atomic-write (atomic temp+rename confined to owner.dataDir)
// ---------------------------------------------------------------------------
const fileLocks = new Map<string, Promise<void>>()

export async function withFileLock<T>(targetPath: string, fn: () => Promise<T> | T): Promise<T> {
  const confined = confinePathToDataDir(targetPath, 'withFileLock')
  const prev = fileLocks.get(confined) ?? Promise.resolve()
  let release!: () => void
  const current = new Promise<void>((res) => { release = res })
  const chain = prev.then(() => current)
  fileLocks.set(confined, chain)
  await prev
  try {
    return await fn()
  } finally {
    release()
    if (fileLocks.get(confined) === chain) {
      fileLocks.delete(confined)
    }
  }
}

export async function writeFileAtomic(
  targetPath: string,
  data: string | Uint8Array,
  options?: { encoding?: BufferEncoding, mode?: number },
): Promise<void> {
  const dataRoot = activeDataDir('writeFileAtomic')
  const confined = confinePathToDataDir(targetPath, 'writeFileAtomic')
  const bytes = typeof data === 'string'
    ? Buffer.from(data, options?.encoding ?? 'utf8')
    : Buffer.from(data.buffer, data.byteOffset, data.byteLength)
  if (bytes.byteLength > MAX_ATOMIC_WRITE_BYTES) {
    throw new RangeError(`writeFileAtomic exceeds the ${MAX_ATOMIC_WRITE_BYTES} byte limit`)
  }
  try {
    const existing = await lstat(confined)
    if (existing.isSymbolicLink()) {
      throw new Error('writeFileAtomic refused symlink target')
    }
  } catch (error: any) {
    if (error?.code !== 'ENOENT') throw error
  }
  const parent = dirname(confined)
  await mkdir(parent, { recursive: true })
  const canonicalParent = canonicalPath(parent)
  const parentKey = insideKey(dataRoot, parent)
  if (parentKey === undefined || (parentKey === '' ? canonicalParent !== dataRoot : !unlinkedKeyMatches(dataRoot, parentKey, canonicalParent))) {
    throw new Error('writeFileAtomic refused symlink escaping plugin dataDir')
  }
  const tmpPath = join(canonicalParent, `.cw-atomic-${process.pid}-${randomBytes(8).toString('hex')}.tmp`)
  try {
    await writeFile(tmpPath, bytes, {
      flag: 'wx',
      ...(options?.mode !== undefined ? { mode: options.mode } : {}),
    })
    await rename(tmpPath, confined)
  } catch (error) {
    await rm(tmpPath, { force: true }).catch(() => undefined)
    throw error
  }
}

export const dshAtomicWriteModule = Object.freeze({
  writeFileAtomic,
  withFileLock,
})

// ---------------------------------------------------------------------------
// @deepseek-ai/dsh-skill (read-only skill schema & frontmatter helpers)
// ---------------------------------------------------------------------------
export const BUNDLED_SKILL_RANK = 100

export function isModelInvocable(skill: unknown): boolean {
  if (!skill || typeof skill !== 'object') return false
  const s = skill as Record<string, unknown>
  if (s.disableModelInvocation === true || s.modelInvocable === false) return false
  return true
}

export function isUserInvocable(skill: unknown): boolean {
  if (!skill || typeof skill !== 'object') return false
  const s = skill as Record<string, unknown>
  if (s.userInvocable === false) return false
  return true
}

export function parseSkillFrontmatter(markdown: unknown): { frontmatter: Record<string, string>, body: string } {
  if (typeof markdown !== 'string') return { frontmatter: {}, body: '' }
  const match = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/.exec(markdown)
  if (!match) return { frontmatter: {}, body: markdown }
  const frontmatter: Record<string, string> = {}
  for (const line of match[1].split(/\r?\n/)) {
    const idx = line.indexOf(':')
    if (idx <= 0) continue
    const key = line.slice(0, idx).trim()
    const val = line.slice(idx + 1).trim().replace(/^["']|["']$/g, '')
    if (key) frontmatter[key] = val
  }
  return { frontmatter, body: match[2] }
}

export function renderSkillContent(skill: unknown, args?: string): string {
  const raw = typeof skill === 'string'
    ? skill
    : (skill && typeof skill === 'object' && typeof (skill as { content?: unknown }).content === 'string')
      ? (skill as { content: string }).content
      : ''
  const { body } = parseSkillFrontmatter(raw)
  if (typeof args !== 'string' || args.length === 0) return body
  return body.replaceAll('$ARGUMENTS', args)
}

export const dshSkillModule = Object.freeze({
  BUNDLED_SKILL_RANK,
  isModelInvocable,
  isUserInvocable,
  parseSkillFrontmatter,
  renderSkillContent,
})

// ---------------------------------------------------------------------------
// @deepseek-ai/dsh-typert-protocol (pure protocol constants/types/codecs)
// ---------------------------------------------------------------------------
export const Remote = Object.freeze({
  strictCodec<T = unknown>(typeSymbol: string, schema: T) {
    return Object.freeze({
      mode: 'strict' as const,
      typeSymbol,
      create: () => schema,
      schema,
    })
  },
  jsonParameter(name: string, typeSymbol: string, schema: unknown, acceptsUndefined = false) {
    return Object.freeze({
      name,
      wire: name,
      source: 'json' as const,
      codec: Remote.strictCodec(typeSymbol, schema),
      ...(acceptsUndefined ? { acceptsUndefined: true } : {}),
    })
  },
  descriptor(def: Record<string, unknown>) {
    return Object.freeze({ ...def })
  },
})

export class TypertRemoteService extends Service {
  readonly descriptors: readonly unknown[]
  constructor(ctx: any, name: string, descriptors: readonly unknown[] = []) {
    super(ctx, name)
    this.descriptors = Object.freeze([...descriptors])
  }
  getDescriptors(): readonly unknown[] {
    return this.descriptors
  }
}

export const dshTypertProtocolModule = Object.freeze({
  Remote,
  TypertRemoteService,
})

// ---------------------------------------------------------------------------
// Additional pure DSH helper shims observed in survey tarballs
// ---------------------------------------------------------------------------
export function WorkspaceId(id: string): string {
  return id
}

export const dshWorkspaceModule = Object.freeze({
  WorkspaceId,
})

export function isSurfaceEligibleType(type: unknown): boolean {
  return typeof type === 'string' && (
    type === 'message/user'
    || type === 'message/assistant'
    || type === 'tool/call'
    || type === 'tool/result'
  )
}

export function foldSurface(events: readonly unknown[]): readonly unknown[] {
  if (!Array.isArray(events)) return []
  return events.filter((event: any) => event && typeof event === 'object' && isSurfaceEligibleType(event.type))
}

export const dshSessionSurfaceModule = Object.freeze({
  foldSurface,
  isSurfaceEligibleType,
})

export class SubagentError extends HarnessError {
  constructor(message: string, code = 'SUBAGENT_ERROR', options?: ErrorOptions) {
    super(message, code, options)
    this.name = 'SubagentError'
  }
}

export function foldSubagentDescriptor(events: readonly unknown[]): Record<string, unknown> {
  const out: Record<string, unknown> = { status: 'idle', count: 0 }
  if (!Array.isArray(events)) return out
  for (const ev of events as any[]) {
    if (!ev || typeof ev !== 'object') continue
    if (ev.type === 'subagent/start') {
      out.status = 'running'
      out.count = (out.count as number) + 1
    } else if (ev.type === 'subagent/end') {
      out.status = 'completed'
    }
  }
  return out
}

export function snapshotSubagentDescriptor(descriptor: unknown): Record<string, unknown> {
  if (!descriptor || typeof descriptor !== 'object') return { status: 'idle' }
  return structuredClone(descriptor as Record<string, unknown>)
}

export const dshSubagentModule = Object.freeze({
  SubagentError,
  foldSubagentDescriptor,
  snapshotSubagentDescriptor,
})

export function assembleContextFor(messages: readonly unknown[] = []): readonly unknown[] {
  return Array.isArray(messages) ? [...messages] : []
}

export function installModelSelection(_ctx: unknown, _options?: unknown): () => void {
  return () => undefined
}

export const dshAgentModule = Object.freeze({
  assembleContextFor,
  installModelSelection,
})

export function getDshRuntimeVersion(): string {
  return compatibility.target_dsh_version
}

export function loadProfileDirectory(): never {
  throw new RpcError(ErrorCode.NotAvailable, 'dsh-app-boot profile directory loading is not available in Codewhale')
}

export function resolveBundleDir(): never {
  throw new RpcError(ErrorCode.NotAvailable, 'dsh-app-boot bundle resolution is not available in Codewhale')
}

export async function createRuntimeResolution(): Promise<never> {
  throw new RpcError(ErrorCode.NotAvailable, 'dsh-app-boot runtime resolution is not available in Codewhale')
}

export const dshAppBootModule = Object.freeze({
  getDshRuntimeVersion,
  loadProfileDirectory,
  resolveBundleDir,
  createRuntimeResolution,
})
