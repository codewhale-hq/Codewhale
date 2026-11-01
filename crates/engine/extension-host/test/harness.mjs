// A minimal fake core: spawns the committed host bundle and speaks CWX1 to it.
// Tests import only dist/ — no type stripping, no install.
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { FrameDecoder, encodeFrame, validateMessage } from '../dist/protocol.mjs'

const here = dirname(fileURLToPath(import.meta.url))
export const BUNDLE = join(here, '..', 'dist', 'codewhale-extension-host.mjs')
export const FIXTURES = join(here, '..', '..', 'tests', 'fixtures', 'extension_host')

export function sha256File(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex')
}

/** The runtime running these tests also runs the host: `node --test` or `bun test`. */
export const IS_BUN = typeof process.versions.bun === 'string'

/**
 * The runtime flags and environment the Rust core passes
 * (`supervisor::runtime_args` / `runtime_env`); keep them in sync. Bun ignores
 * Node's heap and `__proto__` flags, so it gets its own: never auto-install
 * packages, ignore `bunfig.toml` and `.env` files in the working directory,
 * and no ShadowRealm. Node switches off `node:sqlite`, and `node:ffi` where
 * this Node has it. Both get a blank `NODE_OPTIONS`, so an inherited preload
 * cannot run before the native-code lockdown.
 */
export const HOST_ARGS = IS_BUN
  ? ['--no-install', '--no-env-file', `--config=${process.platform === 'win32' ? 'NUL' : '/dev/null'}`, '--no-addons']
  : [
      '--max-old-space-size=256',
      '--disable-proto=throw',
      '--no-addons',
      ...(process.platform === 'win32' ? ['--preserve-symlinks', '--preserve-symlinks-main'] : []),
      '--no-experimental-sqlite',
      ...(process.allowedNodeEnvironmentFlags.has('--no-experimental-ffi') ? ['--no-experimental-ffi'] : []),
    ]
export const HOST_ENV = { NODE_OPTIONS: '', ...(IS_BUN ? { BUN_JSC_useShadowRealm: '0', BUN_OPTIONS: '', BUN_BE_BUN: '0' } : {}) }

export const LIMITS = { max_frame: 32 * 1024 * 1024, max_inflight: 256, dispose_deadline_ms: 2000, activate_deadline_ms: 5000 }

/**
 * Start a host. `admit(spec, owner)` decides `registry/register`: return a
 * handle number or `{ refused }`. Registry traffic is recorded in `registry`.
 * `tier` is the trust tier it serves (`--tier=`, after the bundle, as the Rust
 * core passes it); `null` passes none, as a host started by hand would.
 * `coreCall(params, id)` answers a `core/call` request: return a tool result, or
 * `{ error: { code, message } }`; it may be async, and a request the host
 * cancelled is not answered. Requests are recorded in `coreCalls`, the host's
 * `$/cancel` ids in `cancels`.
 */
export async function startHost({ admit, env, ownGroup = false, tier = 'plugin', coreCall, checkpoint, userQuestions, compiledHost, cwd } = {}) {
  const started = performance.now()
  // `ownGroup` spawns the host as a process-group leader and tells it so, as
  // the Rust core does on Unix.
  const child = spawn(compiledHost ?? process.execPath, [...(compiledHost ? [] : [...HOST_ARGS, BUNDLE]), ...(tier === null ? [] : [`--tier=${tier}`])], {
    stdio: ['pipe', 'pipe', 'pipe'],
    cwd,
    env: { ...process.env, ...HOST_ENV, ...env, ...(compiledHost ? { NODE_OPTIONS: '', BUN_OPTIONS: '', BUN_BE_BUN: '0', BUN_JSC_useShadowRealm: '0' } : {}), ...(ownGroup ? { CODEWHALE_HOST_PROCESS_GROUP: '1' } : {}) },
    detached: ownGroup,
  })
  const decoder = new FrameDecoder()
  const pending = new Map()
  const waiters = []
  let stopping = false
  const host = {
    child,
    stderr: '',
    logs: [],
    faulted: [],
    coreCalls: [],
    checkpoints: [],
    questions: [],
    cancels: [],
    registry: [],
    hello: null,
    nextId: 1,
    nextHandle: 1,
    exit: new Promise((resolve) => child.on('exit', (code, signal) => resolve({ code, signal }))),
    send(message) {
      // Shutdown abandons queued fake-core replies; never write them to stdin
      // after `stop()` has ended the stream (Windows reports this as an error).
      if (stopping || child.exitCode !== null || child.signalCode !== null || child.stdin.destroyed || child.stdin.writableEnded) return false
      validateMessage(message, 'core_to_host', tier ?? 'plugin')
      return child.stdin.write(encodeFrame(message))
    },
    request(method, params) {
      const id = host.nextId++
      const promise = new Promise((resolve, reject) => pending.set(id, { resolve, reject }))
      host.send({ jsonrpc: '2.0', id, method, params })
      return { id, promise }
    },
    call(method, params) {
      return host.request(method, params).promise
    },
    cancel(id) {
      host.send({ jsonrpc: '2.0', method: '$/cancel', params: { id } })
    },
    waitFor(predicate, timeoutMs = 5000) {
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('timed out waiting for host message')), timeoutMs)
        waiters.push({ predicate, resolve: (value) => { clearTimeout(timer); resolve(value) } })
      })
    },
    async stop() {
      if (child.exitCode === null && child.signalCode === null) {
        // The child can still have a core/call response queued on the fake
        // core while it handles stdin EOF. Those replies are intentionally
        // abandoned once shutdown starts.
        stopping = true
        child.stdin.end()
        await host.exit
      }
    },
  }
  child.stderr.on('data', (chunk) => { host.stderr += chunk.toString() })
  child.stdout.on('data', (chunk) => {
    for (const raw of decoder.push(chunk)) {
      const message = validateMessage(raw, 'host_to_core', tier ?? 'plugin')
      for (let i = waiters.length - 1; i >= 0; i--) {
        if (waiters[i].predicate(message)) waiters.splice(i, 1)[0].resolve(message)
      }
      if ('method' in message) {
        switch (message.method) {
          case 'host/hello':
            host.hello = message.params
            break
          case 'log':
            host.logs.push(message.params)
            break
          case 'ext/faulted':
            host.faulted.push(message.params)
            break
          case 'registry/register': {
            host.registry.push({ op: 'register', ...message.params })
            const verdict = admit ? admit(message.params.spec, message.params.owner) : undefined
            const result = verdict && typeof verdict === 'object' ? verdict : { handle: verdict ?? host.nextHandle++ }
            if ('handle' in result) host.registry.at(-1).handle = result.handle
            host.send({ jsonrpc: '2.0', id: message.id, result })
            break
          }
          case '$/cancel':
            host.cancels.push(message.params.id)
            break
          case 'session/checkpoint':
          case 'user-questions/ask':
          case 'core/call': {
            const handler = message.method === 'session/checkpoint' ? checkpoint : message.method === 'user-questions/ask' ? userQuestions : coreCall
            const records = message.method === 'session/checkpoint' ? host.checkpoints : message.method === 'user-questions/ask' ? host.questions : host.coreCalls
            records.push({ id: message.id, ...message.params })
            const answer = handler ? handler(message.params, message.id) : { error: { code: -32002, message: 'no handler for ' + message.method } }
            const id = message.id
            Promise.resolve(answer).then((value) => {
              if (host.cancels.includes(id) || child.exitCode !== null || stopping) return
              if (value && typeof value === 'object' && 'error' in value) host.send({ jsonrpc: '2.0', id, error: value.error })
              else host.send({ jsonrpc: '2.0', id, result: value })
            })
            break
          }
          case 'registry/unregister':
            host.registry.push({ op: 'unregister', ...message.params })
            host.send({ jsonrpc: '2.0', id: message.id, result: {} })
            break
        }
      } else {
        const waiter = pending.get(message.id)
        if (waiter) {
          pending.delete(message.id)
          if ('error' in message) waiter.reject(Object.assign(new Error(message.error.message), { code: message.error.code }))
          else waiter.resolve(message.result)
        }
      }
    }
  })
  await host.waitFor((m) => m.method === 'host/hello')
  const ready = host.waitFor((m) => m.method === 'host/ready')
  await host.call('host/initialize', { protocol: 1, limits: LIMITS })
  await ready
  host.readyMs = performance.now() - started
  return host
}

let token = 0
export function owner(pluginId) {
  token += 1
  return { plugin_id: pluginId, generation: 1, owner_token: `token-${pluginId}-${token}-${'0'.repeat(24)}` }
}

/** `extra` adds or replaces `ext/activate` params, such as `config` and `data_dir`. */
export async function activate(host, fixture, entryPath = join(FIXTURES, fixture, 'index.mjs'), extra = {}) {
  const ref = owner(fixture)
  const result = await host.call('ext/activate', {
    owner: ref,
    plugin_name: fixture,
    entry: { path: entryPath, sha256: sha256File(entryPath) },
    config: {},
    ...extra,
  })
  return { ref, result }
}
