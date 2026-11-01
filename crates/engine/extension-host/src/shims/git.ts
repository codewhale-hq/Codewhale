import childProcess from 'node:child_process'
import { AsyncLocalStorage } from 'node:async_hooks'
import { syncBuiltinESMExports } from 'node:module'
import { resolve } from 'node:path'
import { PassThrough } from 'node:stream'
import { promisify } from 'node:util'
import type { CoreApi } from './core.ts'
import type { Json } from '../protocol.ts'

interface GitInvocation {
  workspace: string
  core: Readonly<CoreApi>
  signal: AbortSignal
  live(): boolean
}

export const gitInvocations = new AsyncLocalStorage<GitInvocation | undefined>()
let installed = false

function queryFor(args: unknown): Record<string, Json> {
  if (!Array.isArray(args) || args.length > 8 || args.some(x => typeof x !== 'string' || x.length > 1024 || /[\x00-\x1f\x7f]/.test(x))) {
    throw new Error('Git review scopes require bounded metadata arguments')
  }
  const argv = args[0] === '--no-optional-locks' ? args.slice(1) : args
  const exact = (...values: string[]) => argv.length === values.length && argv.every((value, index) => value === values[index])
  if (exact('for-each-ref', '--format=%(refname:short)', 'refs/heads')) return { query: 'branches' }
  if (exact('log', '-n', '100', '--format=%H%x09%s')) return { query: 'commits' }
  if (exact('rev-parse', '--show-toplevel')) return { query: 'root' }
  if (argv.length === 4 && argv[0] === 'rev-parse' && argv[1] === '--verify' && argv[2] === '--end-of-options') return { query: 'resolve', rev: argv[3] }
  if (argv.length === 4 && argv[0] === 'rev-list' && argv[1] === '--left-right' && argv[2] === '--count') {
    const refs = argv[3].split('...')
    if (refs.length === 2) return { query: 'ahead_behind', ours: refs[0], theirs: refs[1] }
  }
  if (argv.length === 3 && argv[0] === 'merge-base') return { query: 'merge_base', ours: argv[1], theirs: argv[2] }
  throw new Error('Only read-only Git review metadata is available through the Engine')
}

export function installGitAdapter(inPluginContext: () => boolean): void {
  if (installed) return
  installed = true
  const original = childProcess.execFile
  const adapter = function (...raw: unknown[]): childProcess.ChildProcess {
    if (raw[0] !== 'git' || (!gitInvocations.getStore() && !inPluginContext())) return Reflect.apply(original, childProcess, raw)
    const invocation = gitInvocations.getStore()
    const callback = typeof raw.at(-1) === 'function' ? raw.pop() as (error: unknown, stdout: string | Buffer, stderr: string | Buffer) => void : undefined
    const args = Array.isArray(raw[1]) ? raw[1] : []
    const options = (Array.isArray(raw[1]) ? raw[2] : raw[1]) ?? {}
    const child = new childProcess.ChildProcess()
    const stdout = new PassThrough()
    const stderr = new PassThrough()
    child.stdout = stdout
    child.stderr = stderr
    const cancelled = new AbortController()
    child.kill = () => { cancelled.abort(); return true }
    let timeout: NodeJS.Timeout | undefined
    let userSignal: AbortSignal | undefined
    let gitExit = false
    const run = async () => {
      if (!invocation || !invocation.live() || invocation.signal.aborted) throw new Error('Git review metadata requires a live extension tool invocation')
      if (typeof options !== 'object' || options === null || Array.isArray(options)) throw new TypeError('Invalid Git metadata options')
      const opts = options as Record<string, unknown>
      if (Object.keys(opts).some(key => !['cwd', 'signal', 'encoding', 'maxBuffer', 'timeout', 'windowsHide', 'env'].includes(key))) throw new TypeError('Unsupported Git metadata option')
      if (opts.cwd !== undefined && (typeof opts.cwd !== 'string' || resolve(opts.cwd) !== resolve(invocation.workspace))) throw new Error('Git review metadata is scoped to the calling workspace')
      if (opts.env !== undefined) {
        if (typeof opts.env !== 'object' || opts.env === null || Array.isArray(opts.env)) throw new TypeError('Invalid Git metadata environment')
        for (const [key, value] of Object.entries(opts.env)) {
          if (value === process.env[key] || (key === 'GIT_TERMINAL_PROMPT' && value === '0') || (key === 'GIT_OPTIONAL_LOCKS' && value === '0')) continue
          throw new Error('Git review metadata uses the Engine environment')
        }
      }
      const max = opts.maxBuffer ?? 1024 * 1024
      if (!Number.isSafeInteger(max) || (max as number) < 1 || (max as number) > 1024 * 1024) throw new TypeError('Git metadata maxBuffer must be at most 1 MiB')
      const encoding = opts.encoding ?? 'utf8'
      if (encoding !== 'buffer' && (typeof encoding !== 'string' || !Buffer.isEncoding(encoding))) throw new TypeError('Invalid Git metadata encoding')
      if (opts.signal !== undefined && !(opts.signal instanceof AbortSignal)) throw new TypeError('Invalid Git metadata signal')
      if (opts.timeout !== undefined && (!Number.isSafeInteger(opts.timeout) || (opts.timeout as number) < 0)) throw new TypeError('Invalid Git metadata timeout')
      if (typeof opts.timeout === 'number' && opts.timeout > 0) timeout = setTimeout(() => cancelled.abort(), Math.min(opts.timeout, 10_000))
      userSignal = opts.signal instanceof AbortSignal ? opts.signal : undefined
      const signal = AbortSignal.any([invocation.signal, cancelled.signal, ...(userSignal ? [userSignal] : [])])
      signal.throwIfAborted()
      const result = await invocation.core.call('Git', { action: 'review_scope', ...queryFor(args) }, { signal })
      signal.throwIfAborted()
      if (!invocation.live()) throw new Error('Git metadata invocation ended')
      const value = result.structured
      if (result.isError) throw new Error(`Git review metadata unavailable: ${result.content.slice(0, 300)}`)
      if (value === null || typeof value !== 'object' || Array.isArray(value) || typeof value.stdout !== 'string' || typeof value.stderr !== 'string' || !Number.isInteger(value.exit_code)) {
        throw new Error(result.content.includes('[output truncated') ? 'Git review metadata exceeded the Engine nested-call result limit' : 'Engine did not return complete Git review metadata')
      }
      if (Buffer.byteLength(value.stdout) > (max as number) || Buffer.byteLength(value.stderr) > (max as number)) throw Object.assign(new RangeError('stdout maxBuffer length exceeded'), { code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER' })
      const out = Buffer.from(value.stdout)
      const err = Buffer.from(value.stderr)
      const data = { stdout: encoding === 'buffer' ? out : out.toString(encoding as BufferEncoding), stderr: encoding === 'buffer' ? err : err.toString(encoding as BufferEncoding), code: value.exit_code as number }
      if (data.code !== 0) {
        gitExit = true
        throw Object.assign(new Error(value.stderr || 'Git review metadata failed'), { code: data.code, stdout: data.stdout, stderr: data.stderr })
      }
      return data
    }
    void run().then(data => {
      stdout.end(data.stdout)
      stderr.end(data.stderr)
      Object.defineProperty(child, 'exitCode', { value: 0, configurable: true })
      callback?.(null, data.stdout, data.stderr)
      child.emit('exit', 0, null)
      child.emit('close', 0, null)
    }, failure => {
      let error = failure instanceof Error ? failure : new Error(String(failure))
      const verdict = (error as { code?: unknown }).code
      const decided = verdict === 'denied' || verdict === 'refused' || verdict === 'unavailable'
      if (!gitExit && !decided && (cancelled.signal.aborted || invocation?.signal.aborted || userSignal?.aborted)) {
        const killed = cancelled.signal.aborted && !invocation?.signal.aborted && !userSignal?.aborted
        error = killed
          ? Object.assign(new Error('Command failed: git'), { killed: true, signal: 'SIGTERM', code: null, cause: failure })
          : Object.assign(new Error('The operation was aborted', { cause: failure }), { name: 'AbortError', code: 'ABORT_ERR' })
      }
      const detail = error as Error & { code?: string | number | null; stdout?: string | Buffer; stderr?: string | Buffer }
      if (detail.code === undefined) detail.code = 'CW_GIT_REFUSED'
      detail.stdout ??= ''
      detail.stderr ??= ''
      stdout.end(detail.stdout ?? '')
      stderr.end(detail.stderr ?? '')
      if (callback) callback(error, detail.stdout ?? '', detail.stderr ?? '')
      if (gitExit) {
        Object.defineProperty(child, 'exitCode', { value: detail.code, configurable: true })
        child.emit('exit', detail.code as number, null)
        child.emit('close', detail.code as number, null)
      } else {
        if (!callback && child.listenerCount('error') > 0) child.emit('error', error)
        child.emit('close', null, null)
      }
    }).finally(() => { if (timeout !== undefined) clearTimeout(timeout) })
    return child
  }
  Object.defineProperty(adapter, promisify.custom, { value: (...raw: unknown[]) => {
    let child: childProcess.ChildProcess
    const promise = new Promise((resolveResult, reject) => {
      child = adapter(...raw, (error: unknown, stdout: string | Buffer, stderr: string | Buffer) => error ? reject(error) : resolveResult({ stdout, stderr }))
    })
    return Object.assign(promise, { child: child! })
  } })
  childProcess.execFile = adapter as typeof childProcess.execFile
  syncBuiltinESMExports()
}
