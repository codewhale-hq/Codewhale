/**
 * Which JavaScript runtime this host runs on, its kernel memory limit on
 * macOS, and the in-process native-code policy. Everything here runs before
 * any plugin code loads.
 *
 * The host runs on Node (the default) or Bun (an opt-in). Bun emulates
 * `process.versions.node`, so the runtime is read from `process.versions.bun`
 * first: `host/hello` must report what is actually running.
 */
import * as nodeModule from 'node:module'
import * as vm from 'node:vm'

const require = nodeModule.createRequire(import.meta.url)

export interface RuntimeInfo {
  name: 'bun' | 'node'
  version: string
}

export const RUNTIME: RuntimeInfo =
  typeof process.versions.bun === 'string'
    ? { name: 'bun', version: process.versions.bun }
    : { name: 'node', version: process.versions.node }

function refuse(what: string): never {
  throw new Error(`${what} is not available to extensions`)
}

// ---------------------------------------------------------------------------
// Kernel memory limit (macOS, Bun)
// ---------------------------------------------------------------------------

/** Set by the Rust core when it wants this host to limit itself, in MiB. */
const LIMIT_REQUEST = 'CODEWHALE_HOST_MEMORY_LIMIT_MIB'
/** Set only by the re-exec below, never by the core. */
const LIMIT_APPLIED = 'CODEWHALE_HOST_MEMORY_LIMIT_APPLIED'
/** Compile-time image mode; never an environment-controlled launch decision. */
declare const CODEWHALE_COMPILED_HOST: boolean | undefined

const POSIX_SPAWN_SETEXEC = 0x0040
const POSIX_SPAWN_JETSAM_MEMLIMIT_ACTIVE_FATAL = 0x04
const POSIX_SPAWN_JETSAM_MEMLIMIT_INACTIVE_FATAL = 0x08
/** `memorystatus_update` reads -1 as "the default jetsam priority". */
const JETSAM_PRIORITY_DEFAULT = -1

/**
 * Apply the memory limit the core asked for, and return it in MiB once it
 * holds (or `undefined`).
 *
 * macOS has no unprivileged `setrlimit` that bounds memory: `RLIMIT_AS` and
 * `RLIMIT_DATA` below the current mapping size fail with `EINVAL`. A fatal
 * per-process jetsam limit does work unprivileged, but only as a
 * `posix_spawn` attribute (`posix_spawnattr_setjetsam_ext`, libSystem SPI),
 * and any later `exec` clears it, so the core cannot set it on
 * `sandbox-exec`. So the host re-executes itself in place
 * (`POSIX_SPAWN_SETEXEC`: same pid, process group, stdio and Seatbelt
 * sandbox) with the limit set, before any plugin code runs. Past the limit
 * the kernel SIGKILLs the host. That needs FFI, so only Bun does this; the
 * native-code lockdown takes FFI away afterwards (`denyNativeCode`).
 *
 * If the limit cannot be applied, the reason goes to stderr and `host/hello`
 * reports no limit, so the core refuses initialization before plugins load.
 */
export function applyMemoryLimit(): number | undefined {
  const requested = Number(process.env[LIMIT_REQUEST] ?? '')
  const applied = process.env[LIMIT_APPLIED]
  delete process.env[LIMIT_REQUEST]
  delete process.env[LIMIT_APPLIED]
  if (!Number.isSafeInteger(requested) || requested <= 0) return undefined
  if (applied === String(requested)) return requested
  let failure: string
  if (applied !== undefined) {
    failure = `re-exec reported ${applied} MiB, not ${requested}`
  } else if (RUNTIME.name !== 'bun' || process.platform !== 'darwin') {
    failure = `only Bun on macOS applies its own limit (this is ${RUNTIME.name} on ${process.platform})`
  } else {
    // Returns only if the re-exec failed.
    failure = execUnderJetsamLimit(requested)
  }
  process.stderr.write(`codewhale-extension-host: kernel memory limit not applied: ${failure}\n`)
  return undefined
}

function execUnderJetsamLimit(mib: number): string {
  const { dlopen, FFIType, ptr } = require('bun:ffi')
  let lib: any
  try {
    lib = dlopen('/usr/lib/libSystem.B.dylib', {
      posix_spawnattr_init: { args: [FFIType.ptr], returns: FFIType.i32 },
      posix_spawnattr_destroy: { args: [FFIType.ptr], returns: FFIType.i32 },
      posix_spawnattr_setflags: { args: [FFIType.ptr, FFIType.i16], returns: FFIType.i32 },
      posix_spawnattr_setjetsam_ext: {
        args: [FFIType.ptr, FFIType.i16, FFIType.i32, FFIType.i32, FFIType.i32],
        returns: FFIType.i32,
      },
      posix_spawn: {
        args: [FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr],
        returns: FFIType.i32,
      },
    })
  } catch (error) {
    return `libSystem: ${(error as Error).message}`
  }
  const call = lib.symbols
  // Every buffer a pointer refers to stays referenced until the call returns.
  const keep: unknown[] = []
  const cString = (text: string) => {
    const bytes = Buffer.from(`${text}\0`)
    keep.push(bytes)
    return ptr(bytes)
  }
  const cArray = (items: string[]) => {
    const array = new BigUint64Array(items.length + 1)
    items.forEach((item, index) => {
      array[index] = BigInt(cString(item))
    })
    keep.push(array)
    return ptr(array)
  }
  const attr = new BigUint64Array(1)
  let code = call.posix_spawnattr_init(ptr(attr))
  if (code !== 0) {
    lib.close()
    return `posix_spawnattr_init failed (${code})`
  }
  try {
    code = call.posix_spawnattr_setflags(ptr(attr), POSIX_SPAWN_SETEXEC)
    if (code !== 0) return `posix_spawnattr_setflags failed (${code})`
    code = call.posix_spawnattr_setjetsam_ext(
      ptr(attr),
      POSIX_SPAWN_JETSAM_MEMLIMIT_ACTIVE_FATAL | POSIX_SPAWN_JETSAM_MEMLIMIT_INACTIVE_FATAL,
      JETSAM_PRIORITY_DEFAULT,
      mib,
      mib,
    )
    if (code !== 0) return `posix_spawnattr_setjetsam_ext failed (${code})`
    // A compiled image embeds its entry and its runtime flags. Repassing the
    // virtual script path or execArgv would turn them into application args.
    const argv = typeof CODEWHALE_COMPILED_HOST === 'boolean' && CODEWHALE_COMPILED_HOST
      ? [process.execPath, ...process.argv.slice(2)]
      : [process.execPath, ...process.execArgv, ...process.argv.slice(1)]
    const env = { ...process.env, [LIMIT_REQUEST]: String(mib), [LIMIT_APPLIED]: String(mib) }
    const envp = Object.entries(env)
      .filter((entry): entry is [string, string] => typeof entry[1] === 'string')
      .map(([key, value]) => `${key}=${value}`)
    code = call.posix_spawn(null, cString(process.execPath), null, ptr(attr), cArray(argv), cArray(envp))
    return `posix_spawn failed (${code})`
  } finally {
    call.posix_spawnattr_destroy(ptr(attr))
    lib.close()
  }
}

// ---------------------------------------------------------------------------
// Native-code policy
// ---------------------------------------------------------------------------

function lockMethod(target: object, name: string, what: string) {
  Object.defineProperty(target, name, {
    value: () => refuse(what),
    writable: false,
    configurable: false,
  })
}

/**
 * Every export becomes a getter that throws, and the object is frozen. A
 * builtin's export object is shared by `import` and `require`, and Bun builds
 * a module's ESM namespace from it at the first `import`, so
 * `import { dlopen } from 'bun:ffi'` fails at link time, and so do
 * `require('bun:ffi')` and a dynamic `import()`. (`verifyBunLockdown` checks
 * that this still holds on the running Bun.)
 */
function lockExports(target: Record<string, unknown>, what: string) {
  for (const name of Object.keys(target)) {
    Object.defineProperty(target, name, {
      get: () => refuse(what),
      enumerable: true,
      configurable: false,
    })
  }
  Object.freeze(target)
}

/**
 * Every method and accessor of a class's prototype throws. For a module whose
 * ESM namespace Bun builds before the host can lock its export object (Bun's
 * `node:sqlite`): the class stays importable, but nothing can be done with it.
 */
function lockPrototype(target: { prototype: object }, what: string) {
  const prototype = target.prototype
  for (const key of Reflect.ownKeys(prototype)) {
    if (key === 'constructor') continue
    const accessor = Object.getOwnPropertyDescriptor(prototype, key)
    Object.defineProperty(
      prototype,
      key,
      accessor?.get || accessor?.set
        ? { get: () => refuse(what), set: () => refuse(what), configurable: false }
        : { value: () => refuse(what), writable: false, configurable: false },
    )
  }
  Object.freeze(prototype)
}

/** A specifier esbuild leaves alone and tsc types as `any`. */
function importBuiltin(specifier: string): Promise<any> {
  return import(specifier)
}

/** A builtin module's export object, or `undefined` where this build lacks it. */
function builtin(specifier: string): any {
  try {
    return require(specifier)
  } catch {
    return undefined
  }
}

/**
 * Fail the host's start if a lock does not hold on this Bun: the locks rely
 * on how Bun builds builtin modules, and a newer Bun may build them
 * differently. `locked` names what `denyNativeCode` found and locked.
 */
async function verifyBunLockdown(locked: Set<string>) {
  const probes: [string, () => Promise<unknown>][] = [
    ['`bun:ffi`', () => importBuiltin('bun:ffi').then((ffi) => ffi.dlopen)],
    ['`Bun.FFI`', async () => (globalThis as any).Bun.FFI.dlopen],
    ['`bun:sqlite`', () => importBuiltin('bun:sqlite').then((sqlite) => sqlite.Database)],
    ['`node:sqlite`', () => importBuiltin('node:sqlite').then(({ DatabaseSync }) => new DatabaseSync(':memory:', { open: false }).open())],
  ]
  for (const [what, probe] of probes) {
    if (!locked.has(what)) continue
    const outcome = await probe().then(
      () => 'reachable',
      (error) => String((error as Error)?.message ?? error),
    )
    if (outcome !== `${what} is not available to extensions`) {
      throw new Error(`native-code lockdown does not hold on Bun ${RUNTIME.version}: ${what} (${outcome})`)
    }
  }
}

/**
 * Take in-process native code away from plugins. Call once, after the host
 * has started its own watchdog Worker and before any plugin loads. The OS
 * sandbox (Seatbelt on macOS) stays the boundary; this is the loader-level
 * policy on top of it, and a process a plugin starts is outside it (it runs
 * under the same sandbox).
 *
 * Both runtimes:
 * - `process.dlopen` loads a shared library in-process (`--no-addons` also
 *   covers it).
 * - `process.execve` would replace the host image, dropping these flags and
 *   the macOS memory limit.
 * - Worker threads: a Worker is a new realm. Under Bun it gets a fresh
 *   `bun:ffi` and `Bun.FFI` that the lockdown below never touched; under Node
 *   an explicit `execArgv` starts it without `--no-experimental-ffi` and
 *   `--no-experimental-sqlite`.
 *
 * Bun: `bun:ffi` and `Bun.FFI` (`dlopen`, `linkSymbols`, raw pointer reads and
 * writes); `bun:sqlite` and `node:sqlite` (`setCustomSQLite` and SQLite
 * extensions load native libraries); and `ShadowRealm`, whose realms import a
 * fresh `bun:ffi`. The launcher disables ShadowRealm engine-wide
 * (`BUN_JSC_useShadowRealm=0`, which also covers `node:vm` contexts). The host
 * refuses to start if ShadowRealm is still there or a lock does not hold.
 *
 * Node: `node:ffi` and `node:sqlite` are switched off by launcher flags; the
 * host refuses to start if either is still a builtin.
 *
 * Known limit: this is a list of the entry points found (Bun 1.4, Node 22 and
 * 26). A native-code entry point a newer runtime adds is not covered until it
 * is added here.
 */
export async function denyNativeCode() {
  lockMethod(process, 'dlopen', 'process.dlopen')
  if (typeof (process as any).execve === 'function') lockMethod(process, 'execve', 'process.execve')

  const denied = function Worker() {
    refuse('`Worker`')
  }
  const threads = require('node:worker_threads')
  Object.defineProperty(threads, 'Worker', { value: denied, writable: false, configurable: false, enumerable: true })
  // Node: refresh an ESM namespace created before this patch. Bun has none
  // yet (the host reads `node:worker_threads` through `require`).
  ;(nodeModule as any).syncBuiltinESMExports?.()
  if (typeof (globalThis as any).Worker === 'function') {
    Object.defineProperty(globalThis, 'Worker', { value: denied, writable: false, configurable: false })
  }

  if (RUNTIME.name === 'bun') {
    // A module this Bun build lacks has nothing to lock; every one that
    // exists is locked and then verified.
    const locked = new Set<string>()
    const ffi = builtin('bun:ffi')
    if (ffi) {
      lockExports(ffi, '`bun:ffi`')
      locked.add('`bun:ffi`')
    }
    const bun = (globalThis as any).Bun
    if (bun.FFI) {
      lockExports(bun.FFI, '`Bun.FFI`')
      // A non-configurable data property can still be made read-only.
      Object.defineProperty(bun, 'FFI', { writable: false })
      locked.add('`Bun.FFI`')
    }
    const bunSqlite = builtin('bun:sqlite')
    if (bunSqlite) {
      lockExports(bunSqlite, '`bun:sqlite`')
      locked.add('`bun:sqlite`')
    }
    const sqlite = builtin('node:sqlite')
    if (sqlite) {
      for (const name of ['DatabaseSync', 'StatementSync', 'Session']) {
        if (typeof sqlite[name] === 'function') lockPrototype(sqlite[name], '`node:sqlite`')
      }
      lockExports(sqlite, '`node:sqlite`')
      locked.add('`node:sqlite`')
    }
    if (typeof (globalThis as any).ShadowRealm !== 'undefined' || vm.runInNewContext('typeof ShadowRealm') !== 'undefined') {
      throw new Error('ShadowRealm is enabled; the host must run with BUN_JSC_useShadowRealm=0')
    }
    await verifyBunLockdown(locked)
    return
  }
  for (const [name, flag] of [
    ['node:ffi', '--no-experimental-ffi'],
    ['node:sqlite', '--no-experimental-sqlite'],
  ]) {
    if (nodeModule.isBuiltin(name)) throw new Error(`\`${name}\` is enabled; the host must run with ${flag}`)
  }
}
