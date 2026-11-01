/**
 * codewhale-extension-host entry point.
 *
 * Boot order matters: stdout is the protocol channel, so it is captured and
 * every other writer is rebound to stderr *before* any plugin code can load.
 */
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import * as cordis from '@deepseek-ai/cordis'
import * as schemastery from '@deepseek-ai/schemastery'
import * as cosmokit from '@deepseek-ai/cosmokit'
import * as dshUtilValues from '@deepseek-ai/dsh-util-values'
import * as dshToolsCompat from './dsh/dsh-tools-compat.js'
import { createUserMessage, SessionId, PERSONA_PREFIX_SECTION, HarnessError, UserQuestionError } from './dsh/dsh-message-compat.js'
import * as dshComposition from './dsh/composition.ts'
import * as dshLoader from './dsh/upstream/loader/src/index.ts'
import * as dshInclude from './dsh/upstream/include/src/index.ts'
import { installResolveHooks } from './dsh/resolve-hooks.ts'
import { ErrorCode, FrameDecoder, encodeFrame, PROTOCOL_VERSION, type Message, type HarnessRunParams, type McpOpenParams, type McpRequestParams, type McpCloseParams } from './protocol.ts'
import { RpcError, RpcPeer } from './rpc.ts'
import { HostRoot, ownerStorage } from './root.ts'
import { RUNTIME, applyMemoryLimit, denyNativeCode } from './runtime.ts'
import { builtinModuleDigests, parseTier } from './tier.ts'
import { windowsSandboxProbe } from './windows-sandbox-probe.mjs'

export const HOST_VERSION = '0.1.0'

// 0a. Which trust tier this process serves (`--tier=plugin|builtin`). Anything
//     else is refused before the host does any work. `process.exit` is still
//     the real one here; step 2 replaces it.
const TIER = (() => {
  try {
    return parseTier(process.argv.slice(2))
  } catch (error) {
    process.stderr.write(`codewhale-extension-host: ${(error as Error).message}\n`)
    return process.exit(64)
  }
})()

// Fixed diagnostic mode is selected only by Core before Native admission. It
// works in the compiled image too, without inventing a second host executable.
if (process.argv.includes('--windows-sandbox-probe')) {
  if (process.platform !== 'win32' || TIER !== 'plugin') process.exit(64)
  try {
    process.stdout.write(JSON.stringify(await windowsSandboxProbe()) + '\n')
    process.exit(0)
  } catch (error) {
    process.stderr.write(`Windows sandbox probe: ${(error as Error).message}\n`)
    process.exit(70)
  }
}

// 0b. The kernel memory limit the core asked for (macOS + Bun). This may
//     re-execute the process in place (argv, and so the tier, are kept), so it
//     runs before anything else that has an effect.
const MEMORY_LIMIT_MIB = applyMemoryLimit()

// 1. Own the protocol channel; rebind every other stdout writer to stderr.
const channelWrite = process.stdout.write.bind(process.stdout)
const stderrWrite = process.stderr.write.bind(process.stderr)
process.stdout.write = ((chunk: any, encoding?: any, callback?: any) =>
  stderrWrite(chunk, encoding, callback)) as typeof process.stdout.write
for (const method of ['log', 'info', 'debug', 'trace', 'dir'] as const) {
  ;(console as any)[method] = (...args: unknown[]) => console.error(...args)
}

// 2. A plugin bug must not be able to end the host. (Not a security control:
//    `process.kill(process.pid)` still works.)
const realExit = process.exit.bind(process)
process.exit = ((code?: number) => {
  throw new Error(`process.exit(${code ?? ''}) is not available to extensions`)
}) as typeof process.exit
process.abort = (() => {
  throw new Error('process.abort() is not available to extensions')
}) as typeof process.abort

// 3. One Cordis, one schemastery, one cosmokit for every plugin.
installResolveHooks({
  cordis: cordis as unknown as Record<string, unknown>,
  schemastery: schemastery as unknown as Record<string, unknown>,
  cosmokit: cosmokit as unknown as Record<string, unknown>,
  'dsh-util-values': dshUtilValues as unknown as Record<string, unknown>,
  'dsh-tools': dshToolsCompat as unknown as Record<string, unknown>,
  'dsh-llm-values': { createUserMessage, HarnessError },
  'dsh-user-questions-values': { UserQuestionError },
  'dsh-session-values': { SessionId },
  'dsh-system-prompt-values': { PERSONA_PREFIX_SECTION },
  'dsh-composition': dshComposition as unknown as Record<string, unknown>,
  'dsh-loader': dshLoader as unknown as Record<string, unknown>,
  'dsh-include': dshInclude as unknown as Record<string, unknown>,
  'dsh-group': { default: dshLoader.Group },
  // `@deepseek-ai/dsh-commands/brand`: the brands are plain strings at runtime.
  'dsh-commands-brand': { CommandDefinitionId: (id: string) => id, CommandId: (id: string) => id },
})

// Only the release compiler defines this constant. Runtime environments never
// supply it: the compiled image still identifies the canonical embedded source.
declare const CODEWHALE_COMPILED_BUNDLE_SHA256: string | undefined

function bundleDigest(): string {
  if (typeof CODEWHALE_COMPILED_BUNDLE_SHA256 === 'string') return CODEWHALE_COMPILED_BUNDLE_SHA256
  try {
    return createHash('sha256').update(readFileSync(fileURLToPath(import.meta.url))).digest('hex')
  } catch {
    return 'unknown'
  }
}

// 3b. When the core goes away, take every process this host started with it.
//     On Unix the core spawns the host as the leader of its own process group
//     and says so; killing the group reaches plugin children (not ones that
//     called `setsid`). On Windows the core's Job Object does this when the
//     core's handle closes.
const OWN_GROUP = process.platform !== 'win32' && process.env.CODEWHALE_HOST_PROCESS_GROUP === '1'

function killHostTree(): never {
  if (OWN_GROUP) {
    try {
      process.kill(-process.pid, 'SIGKILL')
    } catch {
      // Not a group leader after all; exit alone.
    }
  }
  return realExit(0)
}

// A plugin that blocks the event loop would never see stdin EOF, so a
// watchdog on its own thread notices the parent going away (the host is
// re-parented, which PID reuse cannot fake) and kills the tree from there.
// `require`, not `import`: under Bun an ESM import would fix the module's
// namespace before `denyNativeCode` replaces `Worker` for plugins.
const { Worker } = createRequire(import.meta.url)('node:worker_threads') as typeof import('node:worker_threads')
const watchdog = new Worker(
  `const { workerData } = require('node:worker_threads')
  const parent = process.ppid
  setInterval(() => {
    if (process.ppid === parent) return
    try { process.kill(workerData.group ? -workerData.pid : workerData.pid, 'SIGKILL') } catch {}
  }, 500)`,
  // A Worker isolate reserves its own executable code range. On Linux x64
  // with Node 24 that reservation is charged in full to the host's 1 GiB
  // RLIMIT_DATA cap, so a default-sized range aborted the host at startup
  // ("Failed to reserve virtual memory for CodeRange"). This loop needs
  // almost no generated code.
  { eval: true, workerData: { pid: process.pid, group: OWN_GROUP }, resourceLimits: { maxOldGenerationSizeMb: 8, codeRangeSizeMb: 16 } },
)
watchdog.unref()

// 3c. No in-process native code for plugins (`runtime.ts`). After the
//     watchdog, which is the host's only Worker.
await denyNativeCode()

function shutdownNow(code: number) {
  // Let queued frames flush before exiting.
  channelWrite('', () => realExit(code))
  setTimeout(() => realExit(code), 200).unref()
}

const rpc = new RpcPeer((message: Message) => {
  channelWrite(encodeFrame(message))
}, TIER)
const host = new HostRoot(rpc, TIER)
let initialized = false

rpc.onRequest('host/initialize', (params: any) => {
  if (params.protocol !== PROTOCOL_VERSION) {
    stderrWrite(`codewhale-extension-host: core speaks protocol ${params.protocol}, host speaks ${PROTOCOL_VERSION}\n`)
    setImmediate(() => realExit(78))
    throw new RpcError(ErrorCode.InvalidParams, `unsupported protocol ${params.protocol}`)
  }
  initialized = true
  setImmediate(() => rpc.notify('host/ready', {}))
  return {}
})

function requireInitialized() {
  if (!initialized) throw new RpcError(ErrorCode.InvalidRequest, 'host is not initialized')
}

rpc.onRequest('host/ping', () => {
  requireInitialized()
  return {}
})

rpc.onRequest('ext/activate', async (params: any) => {
  requireInitialized()
  return host.activate(params)
})

rpc.onRequest('ext/deactivate', async (params: any) => {
  requireInitialized()
  return host.deactivate(params.owner, params.entry)
})

rpc.onRequest('tool/call', async (params: any, cx) => {
  requireInitialized()
  return host.callTool(params.handle, params.input, params.call_id, cx.signal, params.workspace, params.ticket, params)
})

rpc.onRequest('command/run', async (params: any, cx) => {
  requireInitialized()
  return host.callCommand(params.handle, params.raw_input, params.command_id, cx.signal, params.workspace, params)
})

rpc.onRequest('hook/evaluate', async (params: any, cx) => {
  requireInitialized()
  return host.evaluateHook(params, cx.signal)
})

rpc.onRequest('harness/run', async (params, cx) => { requireInitialized(); return host.harnessRun(params as HarnessRunParams, cx.signal) })
rpc.onRequest('mcp/open', async (params, cx) => { requireInitialized(); return host.mcpOpen(params as McpOpenParams, cx.signal) })
rpc.onRequest('mcp/request', async (params, cx) => { requireInitialized(); return host.mcpRequest(params as McpRequestParams, cx.signal) })
rpc.onRequest('mcp/close', async (params) => { requireInitialized(); return host.mcpClose(params as McpCloseParams) })

rpc.onRequest('host/shutdown', async () => {
  await host.deactivateAll(2_000)
  setImmediate(() => shutdownNow(0))
  return {}
})

// 4. Faults are attributed to the owning fiber, which is disposed; the host survives.
function onFault(error: unknown) {
  const owner = ownerStorage.getStore()
  const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error)
  if (owner && owner.state !== 'disposed') {
    rpc.notify('ext/faulted', { owner: owner.ref, error: text.slice(0, 4096) })
    void host.disposeOwner(owner).catch(() => undefined)
  } else {
    host.log('error', `unattributed extension fault: ${text}`)
  }
}
process.on('uncaughtException', onFault)
process.on('unhandledRejection', onFault)

// 5. The channel: stdin EOF means the core is gone, whatever the reason.
const decoder = new FrameDecoder()
process.stdin.on('data', (chunk: Buffer) => {
  let messages: unknown[]
  try {
    messages = decoder.push(chunk)
  } catch (error) {
    stderrWrite(`codewhale-extension-host: ${(error as Error).message}\n`)
    realExit(65)
    return
  }
  for (const message of messages) {
    try {
      rpc.handle(message)
    } catch (error) {
      stderrWrite(`codewhale-extension-host: protocol error: ${(error as Error).message}\n`)
      realExit(65)
      return
    }
  }
})
process.stdin.on('end', () => {
  rpc.close('core closed the channel')
  killHostTree()
})

rpc.notify('host/hello', {
  protocol: { min: PROTOCOL_VERSION, max: PROTOCOL_VERSION },
  host_version: HOST_VERSION,
  bundle_sha256: bundleDigest(),
  runtime: RUNTIME,
  tier: TIER,
  builtin_modules: builtinModuleDigests(),
  ...(MEMORY_LIMIT_MIB === undefined ? {} : { memory_limit_mib: MEMORY_LIMIT_MIB }),
})
