/** Owner-local plugin state, never session history or a credential service. */
import { constants } from 'node:fs'
import { lstat, mkdir, open, readdir, realpath, rename, unlink } from 'node:fs/promises'
import { isAbsolute, join } from 'node:path'
import { createHash, randomUUID } from 'node:crypto'
import { setTimeout as delay } from 'node:timers/promises'
import { isJson } from '../json.ts'
import type { Json } from '../protocol.ts'

export const STORAGE_LIMITS = Object.freeze({ keyBytes: 128, valueBytes: 128 * 1024, totalBytes: 4 * 1024 * 1024, keys: 1024 })
const RECORD_NAME = /^[a-f0-9]{64}\.json$/u
const MAX_RECORD_BYTES = STORAGE_LIMITS.valueBytes + STORAGE_LIMITS.keyBytes * 6 + 128
const queues = new Map<string, Promise<void>>()

export interface PluginStorage {
  get(key: string): Promise<Json | undefined>
  set(key: string, value: Json): Promise<void>
  delete(key: string): Promise<boolean>
}

export interface StorageOptions {
  /** The exact directory Rust already assigned to this owner. */
  dataDir: string
  /** Must become false as soon as this owner starts disposal or is revoked. */
  isActive: () => boolean
  /** Optional host diagnostic when directory fsync is unavailable after publication. */
  onWarning?: (message: string) => void
}

export class StorageError extends Error {
  readonly code: 'not_available' | 'invalid' | 'limit' | 'corrupt' | 'io'
  constructor(code: StorageError['code'], message: string) {
    super(message)
    this.code = code
    this.name = 'StorageError'
  }
}

function checkKey(key: string): void {
  if (typeof key !== 'string' || !key.length || key.includes('\0') || Buffer.byteLength(key) > STORAGE_LIMITS.keyBytes) {
    throw new StorageError('invalid', `storage key must contain 1 to ${STORAGE_LIMITS.keyBytes} UTF-8 bytes and no NUL`)
  }
}

/** Reject accessors, symbols, sparse arrays and extra properties before isJson reads values. */
function snapshot(value: unknown): Json {
  if (!isJson(value)) throw new StorageError('invalid', 'storage value must be plain JSON')
  const encoded = JSON.stringify(value)
  if (Buffer.byteLength(encoded) > STORAGE_LIMITS.valueBytes) throw new StorageError('limit', 'storage value exceeds its byte limit')
  return JSON.parse(encoded)
}

function recordName(key: string): string {
  return `${createHash('sha256').update(key).digest('hex')}.json`
}

function fsCode(error: unknown): string | undefined {
  return (error as NodeJS.ErrnoException)?.code
}

// Windows may briefly deny access while another process replaces a record.
// Retry only sharing-related I/O errors; permanent denial still fails closed.
async function retryWindowsSharing<T>(operation: () => Promise<T>, beforeRetry?: () => unknown | Promise<unknown>): Promise<T> {
  for (let retry = 0; ; retry++) {
    if (retry) await beforeRetry?.()
    try { return await operation() } catch (error) {
      if (process.platform !== 'win32' || retry >= 10 || !['EACCES', 'EBUSY', 'EPERM'].includes(fsCode(error) ?? '')) throw error
      await delay((retry + 1) * 50)
    }
  }
}

interface RecordSnapshot { key: string; value: Json; bytes: number }

/** Read one bounded, complete record without following a link. */
async function readRecordOnce(directory: string, name: string): Promise<RecordSnapshot | undefined> {
  const path = join(directory, name)
  let file
  try {
    const before = await lstat(path)
    if (!before.isFile() || before.isSymbolicLink() || before.nlink > 1) throw new StorageError('corrupt', 'plugin storage record is not a single-link regular file')
    file = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0))
    const stat = await file.stat()
    // Another host may atomically replace this key between lstat and open,
    // or unlink the old inode after we opened it. Both complete versions are
    // valid snapshots; retain the no-follow/single-link checks on the version
    // actually opened rather than calling an ordinary replacement corruption.
    const current = await lstat(path).catch((error) => { if (fsCode(error) !== 'ENOENT') throw error; return undefined })
    if (!stat.isFile() || stat.nlink > 1 || current?.isSymbolicLink()) throw new StorageError('corrupt', 'plugin storage record changed while opening')
    if (stat.size > MAX_RECORD_BYTES) throw new StorageError('corrupt', 'plugin storage record exceeds its byte limit')
    const bytes = Buffer.alloc(stat.size + 1)
    let length = 0
    while (length < bytes.length) {
      const read = await file.read(bytes, length, bytes.length - length, length)
      if (!read.bytesRead) break
      length += read.bytesRead
    }
    if (length > stat.size) throw new StorageError('corrupt', 'plugin storage record changed while reading')
    const stored = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length)))
    if (!stored || stored.version !== 1 || Object.keys(stored).length !== 3 || !Object.hasOwn(stored, 'value')) throw new StorageError('corrupt', 'plugin storage record format is invalid')
    checkKey(stored.key)
    if (recordName(stored.key) !== name) throw new StorageError('corrupt', 'plugin storage record does not match its key')
    return { key: stored.key, value: snapshot(stored.value), bytes: length }
  } catch (error) {
    if (fsCode(error) === 'ENOENT') return undefined
    if (error instanceof StorageError) throw new StorageError('corrupt', error.message)
    if (error instanceof SyntaxError || error instanceof TypeError) throw new StorageError('corrupt', 'plugin storage record is invalid; existing state was preserved')
    throw error
  } finally {
    await file?.close()
  }
}

/**
 * One frozen API per owner generation; isActive is checked on every admission
 * and again before publication. Unload never deletes state. A write admitted
 * at that final check may finish its atomic rename during disposal.
 *
 * Completed records are atomic per key. Same-key writes from different host
 * processes are last-writer-wins; different keys cannot overwrite one another.
 * A shared in-process queue checks the observed 4 MiB / 1024-record quota before
 * each set. This is not a strict shared disk cap: independent-process races,
 * pending files and crash-orphaned temporary files are outside that accounting.
 * Key/value limits remain strict. No lock survives a crash, no orphan is guessed
 * away, and reads/deletes remain usable when the observed quota is exceeded.
 * This API does not contain arbitrary co-resident native plugin code.
 */
export function createStorage({ dataDir, isActive, onWarning }: StorageOptions): PluginStorage {
  if (typeof dataDir !== 'string' || !isAbsolute(dataDir)) throw new StorageError('invalid', 'plugin storage needs its assigned absolute dataDir')

  function active(): void {
    if (!isActive()) throw new StorageError('not_available', 'plugin storage owner is no longer active')
  }

  async function readRecord(directory: string, name: string): Promise<RecordSnapshot | undefined> {
    return retryWindowsSharing(() => readRecordOnce(directory, name), active)
  }

  async function directory(): Promise<string> {
    const stat = await lstat(dataDir)
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new StorageError('invalid', 'plugin storage dataDir must be a real directory')
    return join(await realpath(dataDir), 'storage-v1')
  }

  async function prepare(directory: string): Promise<void> {
    await mkdir(directory, { mode: 0o700 }).catch((error) => { if (fsCode(error) !== 'EEXIST') throw error })
    const stat = await lstat(directory)
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new StorageError('corrupt', 'plugin storage record directory must be a real directory')
    active()
  }

  async function syncDirectory(directory: string): Promise<void> {
    // File fsync is required before publication. Directory fsync is OS-best-
    // effort after publication; reporting failure as a rejected set would lie
    // about the complete record that already replaced the previous version.
    let dir
    try { dir = await open(directory, constants.O_RDONLY); await dir.sync() } catch (error) {
      try { onWarning?.(`plugin storage record committed; directory fsync unavailable (${fsCode(error) ?? 'unknown filesystem error'})`) } catch { /* Diagnostics cannot undo publication. */ }
    } finally {
      await dir?.close().catch(() => undefined)
    }
  }

  async function checkQuota(directory: string, name: string, candidateBytes: number): Promise<void> {
    const names = (await readdir(directory)).filter((entry) => RECORD_NAME.test(entry))
    let count = 0, bytes = 0
    for (const entry of names) {
      if (entry === name) continue
      const record = await readRecord(directory, entry)
      if (!record) continue // A separate process may have deleted this key.
      count++
      bytes += record.bytes
      if (count + 1 > STORAGE_LIMITS.keys || bytes + candidateBytes > STORAGE_LIMITS.totalBytes) throw new StorageError('limit', 'plugin storage exceeds its observed owner quota')
    }
    if (count + 1 > STORAGE_LIMITS.keys || bytes + candidateBytes > STORAGE_LIMITS.totalBytes) throw new StorageError('limit', 'plugin storage exceeds its observed owner quota')
  }

  async function write(directory: string, key: string, value: Json): Promise<void> {
    const name = recordName(key)
    const encoded = JSON.stringify({ version: 1, key, value }) + '\n'
    // Refuse silent replacement of a corrupt or linked record.
    await readRecord(directory, name)
    await checkQuota(directory, name, Buffer.byteLength(encoded))
    const temporary = join(directory, `.pending-${randomUUID()}.tmp`)
    let file
    let published = false
    try {
      file = await open(temporary, 'wx', 0o600)
      await file.writeFile(encoded)
      await file.sync()
      await file.close()
      file = undefined
      await retryWindowsSharing(async () => {
        // Revalidation and publication share one bounded retry budget. A
        // sharing error while checking the destination must not escape it.
        active()
        await readRecordOnce(directory, name)
        active()
        await rename(temporary, join(directory, name))
      })
      published = true
      await syncDirectory(directory)
    } finally {
      await file?.close()
      // This invocation's unpredictable file only; never remove crash orphans.
      if (!published) await unlink(temporary).catch((error) => { if (fsCode(error) !== 'ENOENT') throw error })
    }
  }

  async function queue<T>(operation: (directory: string) => Promise<T>): Promise<T> {
    active()
    try {
      const dir = await directory()
      const next = (queues.get(dir) ?? Promise.resolve()).then(async () => { active(); await prepare(dir); return operation(dir) })
      const tail = next.then(() => undefined, () => undefined)
      queues.set(dir, tail)
      void tail.then(() => { if (queues.get(dir) === tail) queues.delete(dir) })
      return await next
    } catch (error) {
      if (error instanceof StorageError) throw error
      throw new StorageError('io', `plugin storage operation failed (${fsCode(error) ?? 'unknown filesystem error'})`)
    }
  }

  return Object.freeze({
    get(key: string) {
      return queue(async (dir) => { checkKey(key); const record = await readRecord(dir, recordName(key)); active(); return record?.value })
    },
    set(key: string, value: Json) {
      let copy: Json
      try { active(); checkKey(key); copy = snapshot(value) } catch (error) { return Promise.reject(error) }
      return queue((dir) => write(dir, key, copy))
    },
    delete(key: string) {
      return queue(async (dir) => {
        checkKey(key)
        const path = join(dir, recordName(key))
        const stat = await lstat(path).catch((error) => { if (fsCode(error) !== 'ENOENT') throw error; return undefined })
        if (!stat) return false
        if (!stat.isFile() || stat.isSymbolicLink() || stat.nlink !== 1) throw new StorageError('corrupt', 'plugin storage record is not a single-link regular file')
        active()
        await unlink(path)
        await syncDirectory(dir)
        return true
      })
    },
  })
}
