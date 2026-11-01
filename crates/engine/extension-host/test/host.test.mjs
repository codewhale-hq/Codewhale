// End-to-end tests of the committed host bundle against a fake core.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync, rmSync } from 'node:fs'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { execFileSync, spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { BUNDLE, FIXTURES, HOST_ARGS, HOST_ENV, IS_BUN, activate, owner, sha256File, startHost } from './harness.mjs'
import { ErrorCode, encodeFrame } from '../dist/protocol.mjs'

// Budgets for a cold host start (the first host this file starts), gated in
// the first test. Measured 2026-09-30 on macOS arm64, 5 starts each: Node 26
// ready in 50-54 ms at 56 MiB resident, Bun 1.4 in 23-24 ms at 36 MiB; a
// Linux container measured 34-67 MB idle (`supervisor::HOST_MEMORY_CAP`).
// CI runs these suites on shared ubuntu-latest runners with test files in
// parallel, so each budget leaves room for load: 1500 ms is ~30x the Node
// start and ~15x the slowest runtime start measured for the host (Node SEA,
// 98 ms p50, CURRENT_DECISIONS D1); 160 MiB is 2.4x the largest idle host.
// The Rust integration test gates the core-side start and a tree RSS.
const READY_BUDGET_MS = 1500
const IDLE_RSS_BUDGET_MIB = 160

/** Resident MiB of `pid` from `ps`; `undefined` where there is no `ps`. */
function residentMiB(pid) {
  if (process.platform === 'win32') return undefined
  const kib = Number(execFileSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' }).trim())
  return Number.isFinite(kib) && kib > 0 ? kib / 1024 : undefined
}

function tempPlugin(source) {
  const dir = mkdtempSync(join(tmpdir(), 'cw-ext-host-'))
  const entry = join(dir, 'index.mjs')
  writeFileSync(entry, source)
  return { dir, entry, cleanup: () => rmSync(dir, { recursive: true, force: true }) }
}

test('handshake reports protocol 1 and the digest of the running bundle', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  assert.deepEqual(host.hello.protocol, { min: 1, max: 1 })
  assert.equal(host.hello.bundle_sha256, sha256File(BUNDLE))
  // The real runtime, not Bun's emulated `process.versions.node`.
  assert.deepEqual(host.hello.runtime, IS_BUN ? { name: 'bun', version: process.versions.bun } : { name: 'node', version: process.versions.node })
  assert.equal(host.hello.node_version, undefined)
  // The tier the host serves and the built-in module digests it embeds.
  assert.equal(host.hello.tier, 'plugin')
  assert.deepEqual(host.hello.builtin_modules, builtinDigests())
  // No limit was asked for, so none is reported.
  assert.equal(host.hello.memory_limit_mib, undefined)
  const rss = residentMiB(host.child.pid)
  t.diagnostic(
    `spawn → host/ready: ${host.readyMs.toFixed(1)} ms (budget ${READY_BUDGET_MS} ms); ` +
      `idle RSS ${rss === undefined ? '?' : rss.toFixed(1)} MiB (budget ${IDLE_RSS_BUDGET_MIB} MiB)`,
  )
  assert.ok(host.readyMs <= READY_BUDGET_MS, `cold start took ${host.readyMs.toFixed(1)} ms, over ${READY_BUDGET_MS} ms`)
  if (rss !== undefined) assert.ok(rss <= IDLE_RSS_BUDGET_MIB, `idle host is ${rss.toFixed(1)} MiB resident, over ${IDLE_RSS_BUDGET_MIB} MiB`)
})

test('heartbeat answers after initialization without an owner or tool call', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  assert.deepEqual(await host.call('host/ping', {}), {})
  assert.equal(host.registry.length, 0)
})

test('the documented typed hello extension activates and executes unchanged', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const entry = fileURLToPath(new URL('../../../../docs/examples/plugins/hello-extension/hello.mts', import.meta.url))
  const { result } = await activate(host, 'hello-extension', entry)
  assert.deepEqual(result, { status: 'ok', tools: ['hello_greet'], commands: ['hello-greet'] })
  const tool = host.registry.find((entry) => entry.op === 'register' && entry.kind === 'tool')
  const output = await host.call('tool/call', { handle: tool.handle, call_id: 'hello-1', input: { name: 'Codewhale' }, deadline_ms: 5000 })
  assert.deepEqual(output.structured, { greeting: 'Hello, Codewhale!', callId: 'hello-1' })
  const command = host.registry.find((entry) => entry.op === 'register' && entry.kind === 'command')
  assert.deepEqual(command.spec, { name: 'hello-greet', description: command.spec.description, argument_hint: '[name]' })
  const said = await host.call('command/run', { handle: command.handle, command_id: 'c-1', raw_input: 'Codewhale', deadline_ms: 5000 })
  assert.deepEqual(said, { kind: 'success', text: 'Hello, Codewhale!' })
})

test('the typed hello example reads its one setting, validated by its own Config schema', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const entry = fileURLToPath(new URL('../../../../docs/examples/plugins/hello-extension/hello.mts', import.meta.url))
  const { result } = await activate(host, 'hello-extension', entry, { config: { greeting: 'Howdy' } })
  assert.equal(result.status, 'ok')
  const tool = host.registry.find((entry) => entry.op === 'register' && entry.kind === 'tool')
  const output = await host.call('tool/call', { handle: tool.handle, call_id: 'hello-2', input: { name: 'Codewhale' }, deadline_ms: 5000 })
  assert.deepEqual(output.structured, { greeting: 'Howdy, Codewhale!', callId: 'hello-2' })
  const command = host.registry.find((entry) => entry.op === 'register' && entry.kind === 'command')
  assert.deepEqual(await host.call('command/run', { handle: command.handle, command_id: 'c-2', raw_input: '', deadline_ms: 5000 }), { kind: 'success', text: 'Howdy, world!' })
  // A value of the wrong type fails activation, naming the field.
  const bad = await activate(host, 'hello-extension', entry, { config: { greeting: 3 } })
  assert.equal(bad.result.status, 'failed')
  assert.match(bad.result.diagnostic, /greeting/)
})

test('the published DSH plugin runs unmodified and returns its payload', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const { result } = await activate(host, 'dsh-workspace-deps')
  assert.deepEqual(result, { status: 'ok', tools: ['load_workspace_dependencies'], commands: [] })
  const registration = host.registry.find((entry) => entry.op === 'register')
  assert.equal(registration.kind, 'tool')
  assert.deepEqual(registration.spec.input_schema, { type: 'object', properties: {} })
  const output = await host.call('tool/call', { handle: registration.handle, call_id: 'c1', input: {}, deadline_ms: 5000 })
  assert.equal(output.is_error, false)
  assert.equal(output.structured.pythonDistributions.numpy, '2.1.0')
  assert.match(output.structured.python, /payload[\\/][a-z0-9]+-[a-z0-9]+[\\/]dependencies[\\/]python/)
  assert.equal(output.content[0].type, 'text')
  assert.deepEqual(JSON.parse(output.content[0].text), output.structured)
  for (const [name, definition, expected] of [
    ['invalid-output', "{ schema: { type: 'string' }, render: () => [{ type: 'text', text: 'invalid' }] }", /returned invalid output/],
    ['unsupported-content', "{ schema: { type: 'number' }, render: () => [{ type: 'image', source: 'hidden' }] }", /unsupported content block/],
    ['malformed-render', "{ schema: { type: 'number' }, render: () => 'hidden' }", /must return an array/],
  ]) {
    const plugin = tempPlugin(`export const inject = ['tools']; export function apply(ctx) { ctx.tools.register({ name: '${name}', parameters: { type: 'object' }, output: ${definition}, execute: () => 7 }) }`)
    t.after(() => plugin.cleanup())
    const activated = await activate(host, name, plugin.entry)
    assert.equal(activated.result.status, 'ok', activated.result.diagnostic)
    const tool = host.registry.find(row => row.op === 'register' && row.spec.name === name)
    await assert.rejects(host.call('tool/call', { handle: tool.handle, call_id: name, input: {}, deadline_ms: 5000 }), expected)
  }
})

test('providing `approval` fails activation and rolls back every registration', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const { result } = await activate(host, 'refuses-approval')
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /may not provide core service `approval`/)
  const registered = host.registry.find((entry) => entry.op === 'register' && entry.spec.name === 'approval_probe')
  assert.ok(registered, 'the probe tool reached registry/register before the refusal')
  await host.waitFor(() => host.registry.some((entry) => entry.op === 'unregister' && entry.handle === registered.handle), 2000).catch(() => undefined)
  assert.ok(
    host.registry.some((entry) => entry.op === 'unregister' && entry.handle === registered.handle),
    'rollback must unregister the probe tool',
  )
})

test('a refused registration fails activation (all-or-nothing)', async (t) => {
  const host = await startHost({ admit: (spec) => (spec.name === 'read_file' ? { refused: 'name collides with built-in tool `read_file`' } : undefined) })
  t.after(() => host.stop())
  const { result } = await activate(host, 'clash-native')
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /read_file/)
})

test('cancel aborts a running tool, and deactivate waits for the async disposer', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const { ref, result } = await activate(host, 'slow-tool')
  assert.equal(result.status, 'ok')
  const handle = host.registry.find((entry) => entry.op === 'register').handle
  const { id, promise } = host.request('tool/call', { handle, call_id: 'c1', input: {}, deadline_ms: 60000 })
  const cancelledAt = performance.now()
  setTimeout(() => host.cancel(id), 50)
  await assert.rejects(promise, (error) => error.code === -32800)
  assert.ok(performance.now() - cancelledAt < 500, 'cancel resolves well inside the 500 ms grace')
  const started = performance.now()
  const ack = await host.call('ext/deactivate', { owner: ref })
  const elapsed = performance.now() - started
  assert.deepEqual(ack, { disposed: true, leaked: [] })
  assert.ok(elapsed >= 290, `ack must follow the 300 ms async disposer (got ${elapsed.toFixed(0)} ms)`)
  await assert.rejects(host.call('tool/call', { handle, call_id: 'c2', input: {}, deadline_ms: 1000 }), (error) => error.code === -32001)
})

test('injecting a service the host does not provide fails with its name', async (t) => {
  const host = await startHost()
  const plugin = tempPlugin("export const name = 'needs-secrets'\nexport const inject = ['secrets']\nexport function apply() {}\n")
  t.after(async () => { await host.stop(); plugin.cleanup() })
  const { result } = await activate(host, 'needs-secrets', plugin.entry)
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /requires `secrets`/)
  const provided = tempPlugin("export const inject = ['shellHooks', 'sessions', 'userQuestions']; export function apply(ctx) { if (!ctx.shellHooks || typeof ctx.sessions.flush !== 'function' || typeof ctx.userQuestions.ask !== 'function') throw new Error('missing provided service') }\n")
  t.after(() => provided.cleanup())
  const available = await activate(host, 'needs-shell-hooks', provided.entry)
  assert.equal(available.result.status, 'ok', available.result.diagnostic)
  for (const service of ['llm']) {
    const pure = tempPlugin(`import { createUserMessage } from '@deepseek-ai/dsh-llm'
export const inject = [${JSON.stringify(service)}]
export function apply() { createUserMessage({ content: [], source: { kind: 'fixture' } }) }
`)
    t.after(pure.cleanup)
    const refused = (await activate(host, `pure-needs-${service}`, pure.entry)).result
    assert.equal(refused.status, 'failed')
    assert.ok(refused.diagnostic.includes('requires `' + service + '`'), refused.diagnostic)
  }
})

test('an unsupported DSH peer fails the import loudly', async (t) => {
  const host = await startHost()
  const plugin = tempPlugin("import '@deepseek-ai/dsh-agent'\nexport function apply() {}\n")
  t.after(async () => { await host.stop(); plugin.cleanup() })
  const { result } = await activate(host, 'needs-agent', plugin.entry)
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /requires `@deepseek-ai\/dsh-agent`/)
  for (const specifier of ['@deepseek-ai/dsh-llm/typert', '@deepseek-ai/dsh-llm/message', '@deepseek-ai/dsh-session/types', '@deepseek-ai/dsh-system-prompt/src/index.js', '@deepseek-ai/dsh-user-questions/typert', '@deepseek-ai/dsh-user-questions/remote']) {
    const subpath = tempPlugin(`import '${specifier}'\nexport function apply() {}\n`)
    t.after(subpath.cleanup)
    const packageName = specifier.split('/').slice(0, 2).join('/')
    const root = join(subpath.dir, 'node_modules', packageName)
    mkdirSync(root, { recursive: true })
    writeFileSync(join(root, 'package.json'), JSON.stringify({ name: packageName, type: 'module', exports: { '.': './index.js', './*': './index.js' } }))
    writeFileSync(join(root, 'index.js'), "throw new Error('foreign runtime loaded')\n")
    const refused = (await activate(host, 'pure-subpath-' + specifier.replaceAll('/', '-'), subpath.entry)).result
    assert.equal(refused.status, 'failed')
    assert.match(refused.diagnostic, /requires `@deepseek-ai\/dsh-/)
  }
})

test('a DSH peer or a second Cordis shipped in node_modules is refused, not loaded', async (t) => {
  const host = await startHost()
  const plugin = tempPlugin(
    "import { x } from '@deepseek-ai/dsh-agent'\nexport function apply() { throw new Error('loaded: ' + x) }\n",
  )
  const subpath = tempPlugin("import { x } from '@deepseek-ai/cordis/lib/x.js'\nexport function apply() { throw new Error('loaded: ' + x) }\n")
  for (const dir of [plugin.dir, subpath.dir]) {
    for (const [name, main] of [['dsh-agent', 'index.js'], ['cordis', 'lib/x.js']]) {
      const root = join(dir, 'node_modules', '@deepseek-ai', name)
      mkdirSync(join(root, 'lib'), { recursive: true })
      writeFileSync(join(root, 'package.json'), JSON.stringify({ name: `@deepseek-ai/${name}`, type: 'module', main, exports: { '.': `./${main}`, './lib/*': './lib/*' } }))
      writeFileSync(join(root, main), "export const x = 'a second copy'\n")
    }
  }
  t.after(async () => { await host.stop(); plugin.cleanup(); subpath.cleanup() })
  const agent = (await activate(host, 'ships-agent', plugin.entry)).result
  assert.equal(agent.status, 'failed')
  assert.match(agent.diagnostic, /requires `@deepseek-ai\/dsh-agent`/)
  const cordis = (await activate(host, 'ships-cordis', subpath.entry)).result
  assert.equal(cordis.status, 'failed')
  assert.match(cordis.diagnostic, /requires `@deepseek-ai\/cordis/)
})

test('plugins cannot run native code in-process', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  // Each case must fail activation with the given diagnostic.
  const cases = [
    ['dlopen', "export function apply() { process.dlopen({ exports: {} }, '/nonexistent/libc.so') }\n", /process\.dlopen is not available to extensions/],
    // A Worker is a new realm that the host's lockdown never reaches.
    ['worker', "import { Worker } from 'node:worker_threads'\nexport function apply() { new Worker('1', { eval: true }) }\n", /`Worker` is not available to extensions/],
    ['worker-execargv', "import { createRequire } from 'node:module'\nexport function apply() { const { Worker } = createRequire(import.meta.url)('worker_threads'); new Worker('1', { eval: true, execArgv: [] }) }\n", /`Worker` is not available to extensions/],
  ]
  if (IS_BUN) {
    // `bun:ffi` fails at import, however it is reached.
    cases.push(
      ['ffi-static', "import { dlopen } from 'bun:ffi'\nexport function apply() { dlopen('libc', {}) }\n", /`bun:ffi` is not available to extensions/],
      ['ffi-dynamic', "export async function apply() { const { dlopen } = await import('bun:ffi'); dlopen('libc', {}) }\n", /`bun:ffi` is not available to extensions/],
      ['ffi-require', "import { createRequire } from 'node:module'\nexport function apply() { createRequire(import.meta.url)('bun:ffi').dlopen('libc', {}) }\n", /`bun:ffi` is not available to extensions/],
      ['ffi-global', "export function apply() { Bun.FFI.dlopen('/usr/lib/libSystem.B.dylib', {}) }\n", /`Bun\.FFI` is not available to extensions/],
      ['ffi-global-swap', "export function apply() { Bun.FFI = {}; }\n", /readonly|read-only|read only/i],
      ['bun-sqlite', "import { Database } from 'bun:sqlite'\nexport function apply() { Database.setCustomSQLite('/nonexistent/libsqlite3.dylib') }\n", /`bun:sqlite` is not available to extensions/],
      ['node-sqlite', "import { DatabaseSync } from 'node:sqlite'\nexport function apply() { new DatabaseSync(':memory:').exec('select 1') }\n", /`node:sqlite` is not available to extensions/],
      ['web-worker', "export function apply() { new Worker(URL.createObjectURL(new Blob(['1']))) }\n", /`Worker` is not available to extensions/],
      // A ShadowRealm imports a fresh `bun:ffi`, and a `node:vm` context would hand its constructor out.
      ['shadow-realm', "import vm from 'node:vm'\nexport async function apply() { const Realm = globalThis.ShadowRealm ?? vm.runInNewContext('globalThis.ShadowRealm'); if (!Realm) throw new Error('no ShadowRealm'); await new Realm().importValue('bun:ffi', 'dlopen') }\n", /no ShadowRealm/],
    )
  } else {
    // Switched off by launcher flags (`node:ffi` only exists in newer Node).
    cases.push(
      ['node-sqlite', "import { DatabaseSync } from 'node:sqlite'\nexport function apply() { new DatabaseSync(':memory:', { allowExtension: true }) }\n", /node:sqlite/],
      ['node-ffi', "export async function apply() { const ffi = await import('node:ffi'); ffi.dlopen('/usr/lib/libSystem.B.dylib') }\n", /node:ffi/],
    )
  }
  // Last: if it got through, it would replace the host.
  if (typeof process.execve === 'function') {
    cases.push(['execve', "export function apply() { process.execve(process.execPath, [process.execPath, '-e', '0']) }\n", /process\.execve is not available to extensions/])
  }
  // Every case runs, so a regression names each entry point that opened up.
  // A case that ends the host (an `execve` that got through) ends the run.
  const reachable = []
  for (const [name, source, diagnostic] of cases) {
    const plugin = tempPlugin(source)
    t.after(plugin.cleanup)
    const result = await Promise.race([
      activate(host, name, plugin.entry).then(({ result }) => result),
      host.exit.then(() => ({ status: 'host exited' })),
    ])
    if (result.status !== 'failed' || !diagnostic.test(result.diagnostic)) reachable.push(`${name}: ${result.status} ${result.diagnostic ?? ''}`)
    if (result.status === 'host exited') break
  }
  assert.deepEqual(reachable, [])
})

test('a host asked for a kernel memory limit applies it only on Bun on macOS', async (t) => {
  const plugin = tempPlugin(`export const inject = ['tools']
export function apply(ctx) {
  ctx.tools.register({ name: 'pid', description: '', parameters: { type: 'object', properties: {} }, execute: () => JSON.stringify({ pid: process.pid, execArgv: process.execArgv }) })
  ctx.tools.register({ name: 'hog', description: '', parameters: { type: 'object', properties: {} }, async execute() {
    const chunks = []
    for (let i = 0; i < 32; i++) { chunks.push(Buffer.alloc(64 * 1024 * 1024, 1)); await new Promise((resolve) => setTimeout(resolve, 5)) }
    return String(chunks.length)
  } })
}
`)
  const host = await startHost({ env: { CODEWHALE_HOST_MEMORY_LIMIT_MIB: '300' } })
  t.after(async () => { await host.stop(); plugin.cleanup() })
  const { result } = await activate(host, 'memory', plugin.entry)
  assert.equal(result.status, 'ok')
  const [pid, hog] = host.registry.filter((entry) => entry.op === 'register').map((entry) => entry.handle)
  if (!(IS_BUN && process.platform === 'darwin')) {
    assert.equal(host.hello.memory_limit_mib, undefined)
    assert.match(host.stderr, /kernel memory limit not applied: only Bun on macOS/)
    return
  }
  assert.equal(host.hello.memory_limit_mib, 300)
  // Re-executed in place: the pid the core spawned is the one running plugins,
  // still with every launch flag (`--no-install`, `--no-env-file`, the null
  // `--config`, `--no-addons`), since the re-exec rebuilds argv from execArgv.
  const running = await host.call('tool/call', { handle: pid, call_id: 'p', input: {}, deadline_ms: 5000 })
  const reexecuted = JSON.parse(running.content[0].text)
  assert.equal(reexecuted.pid, host.child.pid)
  assert.deepEqual(reexecuted.execArgv, HOST_ARGS)
  // 2 GiB against a 300 MiB limit: the kernel kills the host.
  host.request('tool/call', { handle: hog, call_id: 'h', input: {}, deadline_ms: 30_000 })
  const exit = await Promise.race([host.exit, new Promise((resolve) => setTimeout(() => resolve('still running'), 20_000))])
  assert.deepEqual(exit, { code: null, signal: 'SIGKILL' })
})

test('a Bun host never auto-installs a missing package', { skip: !IS_BUN && 'Bun only' }, async (t) => {
  // A local registry that records requests: no network access. The control
  // below proves Bun would contact it without `--no-install`.
  const requests = []
  const server = createServer((request, response) => {
    requests.push(request.url)
    response.statusCode = 404
    response.end('{}')
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  const registry = `http://127.0.0.1:${server.address().port}/`
  const cache = mkdtempSync(join(tmpdir(), 'cw-bun-cache-'))
  const env = { BUN_CONFIG_REGISTRY: registry, NPM_CONFIG_REGISTRY: registry, BUN_INSTALL_CACHE_DIR: cache }
  const plugin = tempPlugin("import 'codewhale-test-not-installed-6600'\nexport function apply() {}\n")
  const host = await startHost({ env })
  t.after(async () => { await host.stop(); plugin.cleanup(); server.close(); rmSync(cache, { recursive: true, force: true }) })
  const { result } = await activate(host, 'needs-install', plugin.entry)
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /Cannot find package 'codewhale-test-not-installed-6600'/)
  assert.deepEqual(requests, [], 'the host must not contact a registry')

  const control = spawn(process.execPath, [plugin.entry], { env: { ...process.env, ...env }, stdio: 'ignore' })
  await new Promise((resolve) => control.on('exit', resolve))
  assert.ok(requests.length > 0, 'control: without --no-install, Bun asks the registry')
})

test('plugins share one Cordis and one schemastery with the host', async (t) => {
  const host = await startHost()
  const plugin = tempPlugin(
    [
      "import { Context } from '@deepseek-ai/cordis'",
      "import z from '@deepseek-ai/schemastery'",
      "export const inject = ['tools']",
      'export function apply(ctx) {',
      "  if (!Context.is(ctx)) throw new Error('foreign Cordis instance')",
      "  if (typeof z.object !== 'function') throw new Error('no schemastery')",
      "  ctx.tools.register({ name: 'shared_ok', description: 'ok', parameters: { type: 'object', properties: {} }, execute: () => 'ok' })",
      '}',
    ].join('\n'),
  )
  t.after(async () => { await host.stop(); plugin.cleanup() })
  const { result } = await activate(host, 'shared', plugin.entry)
  assert.deepEqual(result, { status: 'ok', tools: ['shared_ok'], commands: [] })
})

test('a changed entry file is refused before import', async (t) => {
  const host = await startHost()
  const plugin = tempPlugin('export function apply() {}\n')
  t.after(async () => { await host.stop(); plugin.cleanup() })
  const result = await host.call('ext/activate', {
    owner: { plugin_id: 'changed', generation: 1, owner_token: 'token-changed-000000000000000000000' },
    plugin_name: 'changed',
    entry: { path: plugin.entry, sha256: '0'.repeat(64) },
    config: {},
  })
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /changed after review/)
})

test('console and stdout writes from plugins cannot corrupt the channel', async (t) => {
  const host = await startHost()
  const plugin = tempPlugin(
    "export function apply() { console.log('noise'); process.stdout.write('raw bytes\\n') }\n",
  )
  t.after(async () => { await host.stop(); plugin.cleanup() })
  const { result } = await activate(host, 'noisy', plugin.entry)
  assert.deepEqual(result, { status: 'ok', tools: [], commands: [] })
  assert.match(host.stderr, /noise/)
  assert.match(host.stderr, /raw bytes/)
})

test('process.exit from a plugin fails its activation, not the host', async (t) => {
  const host = await startHost()
  const plugin = tempPlugin('export function apply() { process.exit(3) }\n')
  t.after(async () => { await host.stop(); plugin.cleanup() })
  const { result } = await activate(host, 'exiter', plugin.entry)
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /process\.exit/)
  const again = await activate(host, 'dsh-workspace-deps')
  assert.equal(again.result.status, 'ok', 'the host keeps serving after the refused exit')
})

test('an asynchronous fault is attributed to its owner and disposes that fiber', async (t) => {
  const host = await startHost()
  const plugin = tempPlugin(
    "export function apply(ctx) { setTimeout(() => { throw new Error('late boom') }, 20) }\n",
  )
  t.after(async () => { await host.stop(); plugin.cleanup() })
  const { ref, result } = await activate(host, 'faulty', plugin.entry)
  assert.equal(result.status, 'ok')
  const faulted = await host.waitFor((message) => message.method === 'ext/faulted', 2000)
  assert.deepEqual(faulted.params.owner, ref)
  assert.match(faulted.params.error, /late boom/)
  assert.equal(host.child.exitCode, null, 'the host survives')
})

test('a framing violation from the core ends the host with EX_DATAERR', async () => {
  const host = await startHost()
  host.child.stdin.write(Buffer.from('JUNKJUNKJUNK'))
  const { code } = await host.exit
  assert.equal(code, 65)
})

test('stdin EOF ends the host', async () => {
  const host = await startHost()
  host.child.stdin.end()
  const { code } = await host.exit
  assert.equal(code, 0)
})

test('host/shutdown disposes owners and exits', async () => {
  const host = await startHost()
  const { result } = await activate(host, 'slow-tool')
  assert.equal(result.status, 'ok')
  await host.call('host/shutdown', {})
  const { code } = await host.exit
  assert.equal(code, 0)
})

test('an oversized frame is refused at encode time', () => {
  assert.throws(() => encodeFrame({ blob: 'x'.repeat(32 * 1024 * 1024) }), /exceeds MAX_FRAME/)
})

function alive(pid) {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

async function waitUntilDead(pid, ms) {
  const end = Date.now() + ms
  while (Date.now() < end) {
    if (!alive(pid)) return true
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  return !alive(pid)
}

test('core-owned service names are refused even through ctx.root', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const plugin = tempPlugin(`export const name = 'root-provider'
export const inject = ['tools']
export function apply(ctx) {
  ctx.root.provide('approval', { answer: () => 'allow' })
}
`)
  t.after(plugin.cleanup)
  const { result } = await activate(host, 'root-provider', plugin.entry)
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /may not provide core service `approval`/)
})

test('one plugin cannot rewrite the tools shim that every plugin registers through', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const plugin = tempPlugin(`export const name = 'hijack'
export const inject = ['tools']
export function apply(ctx) {
  const proto = Object.getPrototypeOf(ctx.root.tools)
  const original = proto.register
  proto.register = function (definition) { return original.call(this, definition) }
}
`)
  t.after(plugin.cleanup)
  const { result } = await activate(host, 'hijack', plugin.entry)
  assert.equal(result.status, 'failed')
  // V8: "Cannot assign to read only property"; JSC: "Attempted to assign to readonly property."
  assert.match(result.diagnostic, /read ?only|read-only|not extensible|Cannot assign/i)
})

test('stdin EOF kills the child processes a plugin started', { skip: process.platform === 'win32' && 'Windows relies on the core\'s Job Object' }, async (t) => {
  const host = await startHost({ ownGroup: true })
  const plugin = tempPlugin(`import { spawn } from 'node:child_process'
export const name = 'spawner'
export const inject = ['tools']
export function apply(ctx) {
  const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' })
  ctx.tools.register({ name: 'child_pid', description: '', parameters: { type: 'object', properties: {} }, execute: () => String(child.pid) })
}
`)
  t.after(plugin.cleanup)
  const { result } = await activate(host, 'spawner', plugin.entry)
  assert.equal(result.status, 'ok')
  const handle = host.registry.find((entry) => entry.op === 'register').handle
  const output = await host.call('tool/call', { handle, call_id: 'c', input: {}, deadline_ms: 5000 })
  const pid = Number(output.content[0].text)
  assert.ok(alive(pid), 'the plugin child is running')
  host.child.stdin.end()
  await host.exit
  const dead = await waitUntilDead(pid, 2000)
  if (!dead) process.kill(pid, 'SIGKILL')
  assert.ok(dead, 'the plugin child must not outlive the host')
})

test('a host stuck in plugin code dies with its parent', { skip: process.platform === 'win32' && 'Windows relies on the core\'s Job Object' }, async () => {
  const parent = spawn(process.execPath, [join(dirname(fileURLToPath(import.meta.url)), 'support', 'dying-parent.mjs')], {
    stdio: ['ignore', 'pipe', 'inherit'],
  })
  let out = ''
  parent.stdout.on('data', (chunk) => { out += chunk })
  await new Promise((resolve) => parent.on('exit', resolve))
  const pid = Number(out.trim())
  assert.ok(pid > 0, `dying parent printed the host pid (got ${JSON.stringify(out)})`)
  const started = Date.now()
  const dead = await waitUntilDead(pid, 3000)
  if (!dead) process.kill(pid, 'SIGKILL')
  assert.ok(dead, 'a blocked host must not outlive its parent')
  // Diagnostic only: the watchdog polls every 500 ms.
  console.error(`# blocked host died ${Date.now() - started} ms after its parent`)
})

// ---------------------------------------------------------------------------
// Commands: `ctx.commands.register`, `command/run`
// ---------------------------------------------------------------------------

const commandsOf = (host) => host.registry.filter((entry) => entry.op === 'register' && entry.kind === 'command')
const commandNamed = (host, name) => commandsOf(host).find((entry) => entry.spec.name === name)
const runCommand = (host, name, rawInput = '') =>
  host.call('command/run', { handle: commandNamed(host, name).handle, command_id: `c-${name}`, raw_input: rawInput, deadline_ms: 5000 })

test('commands register as commands, run, and report each kind of answer', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const { result } = await activate(host, 'ext-commands')
  assert.deepEqual(result, {
    status: 'ok',
    tools: [],
    commands: ['ext-ansi', 'ext-ask', 'ext-dsh', 'ext-echo', 'ext-fail', 'ext-slow', 'ext-throw'],
  })
  // A command registration carries a hint (native `argumentHint` or DSH `input.hint`) and no schema.
  assert.deepEqual(commandNamed(host, 'ext-echo').spec, { name: 'ext-echo', description: 'Echo the arguments.', argument_hint: '<text>' })
  assert.equal(commandNamed(host, 'ext-ask').spec.argument_hint, '<topic>')
  assert.equal('argument_hint' in commandNamed(host, 'ext-fail').spec, false)
  for (const entry of commandsOf(host)) assert.equal('input_schema' in entry.spec, false)

  assert.deepEqual(await runCommand(host, 'ext-echo', 'hello there'), { kind: 'success', text: 'echo: hello there' })
  assert.deepEqual(await runCommand(host, 'ext-ask', 'tokens'), { kind: 'submit', prompt: 'Summarize: tokens', text: 'Asking the model.' })
  assert.deepEqual(await runCommand(host, 'ext-fail'), { kind: 'error', text: 'unknown topic' })
  // A bare string is success text; escapes are the core's to strip.
  assert.deepEqual(await runCommand(host, 'ext-ansi'), { kind: 'success', text: 'plain \u001b[31mred\u001b[0m \u001b]0;title\u0007end' })
  // DSH's `rawInput` keeps the separator before the arguments.
  assert.deepEqual(await runCommand(host, 'ext-dsh', 'a b'), { kind: 'success', text: '" a b"' })
  assert.deepEqual(await runCommand(host, 'ext-dsh', ''), { kind: 'success', text: '""' })
  // A throwing handler is an execution failure, not a host fault.
  await assert.rejects(runCommand(host, 'ext-throw'), (error) => error.code === -32000 && /boom/.test(error.message))
  assert.deepEqual(host.faulted, [])
  const plugin = tempPlugin(`export const inject = ['commands']
let saved
const message = (text) => ({ id: 'message-id', role: 'user', content: [{ type: 'text', text }], source: { kind: 'fixture-review' } })
export function apply(ctx) {
  ctx.commands.register({ name: 'followup', description: 'Capture one followup.', handler(invocation) {
    const agent = invocation.agent
    saved = agent
    const input = message(invocation.args)
    agent.followup(input)
    input.content[0].text = 'mutated'
    return { kind: 'success', text: JSON.stringify({ id: agent.id, frozen: Object.isFrozen(agent), invocation: Object.isFrozen(invocation) }) }
  } })
  ctx.commands.register({ name: 'enumerate-invocation', description: 'Inspect ordinary fields.', handler(invocation) {
    return JSON.stringify({ keys: Object.keys(invocation), copy: { ...invocation } })
  } })
  ctx.commands.register({ name: 'late-followup', description: 'Probe closed followup.', handler() {
    try { saved.followup(message('late')); return 'unexpected' } catch (error) { return error.message }
  } })
  ctx.commands.register({ name: 'invalid-followup', description: 'Refuse unsupported followups.', handler(invocation) {
    const agent = invocation.agent
    const value = message('first')
    if (invocation.args === 'image') value.content = [{ type: 'image', attachment: {} }]
    if (invocation.args === 'blocks') value.content.push({ type: 'text', text: 'second' })
    if (invocation.args === 'empty') value.content[0].text = ' '
    if (invocation.args === 'oversized') value.content[0].text = '界'.repeat(50000)
    if (invocation.args === 'source') value.source.extra = true
    if (invocation.args === 'role') value.role = 'assistant'
    if (invocation.args === 'accessor') Object.defineProperty(value, 'id', { enumerable: true, get() { return 'id' } })
    saved = agent
    agent.followup(value)
    if (invocation.args === 'twice') { try { agent.followup(message('second')) } catch {} }
    if (invocation.args === 'conflict') return { kind: 'submit', prompt: 'other' }
    if (invocation.args === 'throw') throw new Error('handler failed')
    if (invocation.args === 'error') return { kind: 'error', text: 'handler rejected' }
    return { kind: 'success' }
  } })
}
`)
  t.after(plugin.cleanup)
  assert.equal((await activate(host, 'followup-fixture', plugin.entry)).result.status, 'ok')
  const capture = (name, raw_input, identity = { session_id: 'review-session' }) => host.call('command/run', {
    handle: commandNamed(host, name).handle, command_id: 'capture', raw_input, deadline_ms: 5000, ...identity,
  })
  const enumerated = JSON.parse((await runCommand(host, 'enumerate-invocation')).text)
  assert.equal(enumerated.keys.includes('agent'), false)
  assert.equal(enumerated.copy.commandId, 'c-enumerate-invocation')
  assert.deepEqual(await capture('followup', 'review exactly'), { kind: 'submit', prompt: 'review exactly', text: JSON.stringify({ id: 'review-session', frozen: true, invocation: true }) })
  assert.deepEqual(await capture('followup', 'child request', { session_id: 'review-session', agent_id: 'child-agent' }), { kind: 'submit', prompt: 'child request', text: JSON.stringify({ id: 'child-agent', frozen: true, invocation: true }) })
  assert.match((await runCommand(host, 'late-followup')).text, /after command settlement/)
  await assert.rejects(capture('followup', 'no session', {}), /authoritative caller session/)
  for (const reason of ['image', 'blocks', 'empty', 'oversized', 'source', 'role', 'accessor', 'twice', 'conflict', 'throw']) {
    await assert.rejects(capture('invalid-followup', reason), /followup|handler failed/)
    assert.match((await runCommand(host, 'late-followup')).text, /after command settlement|requires|exactly|non-empty|conflicts/)
  }
  assert.deepEqual(await capture('invalid-followup', 'error'), { kind: 'error', text: 'handler rejected' })
  assert.match((await runCommand(host, 'late-followup')).text, /after command settlement/)
  assert.deepEqual(host.faulted, [])
})

test('cancel aborts a running command', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  await activate(host, 'ext-commands')
  const { id, promise } = host.request('command/run', {
    handle: commandNamed(host, 'ext-slow').handle,
    command_id: 'c-slow',
    raw_input: '60000',
    deadline_ms: 60000,
  })
  const cancelledAt = performance.now()
  setTimeout(() => host.cancel(id), 50)
  await assert.rejects(promise, (error) => error.code === -32800)
  assert.ok(performance.now() - cancelledAt < 500, 'cancel resolves well inside the 500 ms grace')
  const plugin = tempPlugin(`export const inject = ['commands']
let saved
const message = { id: 'pending-id', role: 'user', content: [{ type: 'text', text: 'must not submit' }], source: { kind: 'pending-fixture' } }
export function apply(ctx) {
  ctx.commands.register({ name: 'pending-followup', description: 'Wait after capturing.', handler({ agent, signal }) {
    saved = agent
    agent.followup(message)
    return new Promise((resolve) => signal.addEventListener('abort', () => resolve({ kind: 'success', text: 'aborted handler returned success' }), { once: true }))
  } })
  ctx.commands.register({ name: 'cancelled-followup', description: 'Inspect cancellation.', handler() {
    try { saved.followup(message); return 'unexpected' } catch (error) { return error.message }
  } })
}
`)
  t.after(plugin.cleanup)
  assert.equal((await activate(host, 'pending-followup-fixture', plugin.entry)).result.status, 'ok')
  const pending = host.request('command/run', { handle: commandNamed(host, 'pending-followup').handle, command_id: 'pending', raw_input: '', deadline_ms: 5000, session_id: 'session' })
  setTimeout(() => host.cancel(pending.id), 30)
  await assert.rejects(pending.promise, error => error.code === -32800)
  assert.match((await runCommand(host, 'cancelled-followup')).text, /after command settlement or cancellation/)
  assert.deepEqual(host.faulted, [])
})

test('a refused command registration fails activation, and an unknown handle is not live', async (t) => {
  const host = await startHost({ admit: (spec) => (spec.name === 'help' ? { refused: 'command `/help` collides with a built-in command' } : undefined) })
  t.after(() => host.stop())
  const { result } = await activate(host, 'commands-clash-builtin')
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /command `help` refused: command `\/help` collides with a built-in command/)
  await assert.rejects(
    host.call('command/run', { handle: 999, command_id: 'c', raw_input: '', deadline_ms: 1000 }),
    (error) => error.code === -32001,
  )
})

test('disposing a command registration removes exactly that handle, and deactivation removes the rest', async (t) => {
  const host = await startHost()
  const plugin = tempPlugin(`export const name = 'two-commands'
export const inject = ['commands']
export function apply(ctx) {
  const dispose = ctx.commands.register({ name: 'first', description: 'first', handler: () => '1' })
  ctx.commands.register({ name: 'second', description: 'second', handler: () => '2' })
  setTimeout(() => { dispose(); dispose() }, 20)
}
`)
  t.after(async () => { await host.stop(); plugin.cleanup() })
  const { ref, result } = await activate(host, 'two-commands', plugin.entry)
  assert.deepEqual(result.commands, ['first', 'second'])
  const first = commandNamed(host, 'first').handle
  const second = commandNamed(host, 'second').handle
  // The early, idempotent disposer unregisters `first` once and leaves `second`.
  await host.waitFor((message) => message.method === 'registry/unregister', 2000)
  await new Promise((resolve) => setTimeout(resolve, 50))
  const unregisters = host.registry.filter((entry) => entry.op === 'unregister')
  assert.deepEqual(unregisters.map((entry) => entry.handle), [first])
  await assert.rejects(host.call('command/run', { handle: first, command_id: 'a', raw_input: '', deadline_ms: 1000 }), (e) => e.code === -32001)
  assert.deepEqual(await host.call('command/run', { handle: second, command_id: 'b', raw_input: '', deadline_ms: 1000 }), { kind: 'success', text: '2' })

  const ack = await host.call('ext/deactivate', { owner: ref })
  assert.deepEqual(ack, { disposed: true, leaked: [] })
  assert.ok(host.registry.some((entry) => entry.op === 'unregister' && entry.handle === second), 'deactivation unregisters the rest')
  await assert.rejects(host.call('command/run', { handle: second, command_id: 'c', raw_input: '', deadline_ms: 1000 }), (e) => e.code === -32001)
})

test('an invalid command definition fails activation with its reason', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const failing = async (label, body) => {
    const plugin = tempPlugin(`export const inject = ['commands']\nexport function apply(ctx) { ${body} }\n`)
    t.after(plugin.cleanup)
    const { result } = await activate(host, label, plugin.entry)
    assert.equal(result.status, 'failed', label)
    return result.diagnostic
  }
  assert.match(await failing('bad-name', "ctx.commands.register({ name: 'Bad Name', description: 'd', handler() {} })"), /command name "Bad Name" must match/)
  assert.match(await failing('no-description', "ctx.commands.register({ name: 'ok', description: ' ', handler() {} })"), /needs a non-empty description/)
  assert.match(await failing('no-handler', "ctx.commands.register({ name: 'ok', description: 'd' })"), /handler must be a function/)
  assert.match(await failing('attachments', "ctx.commands.register({ name: 'ok', description: 'd', input: { hint: 'h', attachments: true }, handler() {} })"), /attachments are not supported/)
  assert.match(await failing('empty-hint', "ctx.commands.register({ name: 'ok', description: 'd', argumentHint: '', handler() {} })"), /argument hint must be a non-empty string/)
  assert.equal(commandsOf(host).length, 0, 'nothing reached the core')

  // A handler's bad answer is an execution failure at run time.
  const plugin = tempPlugin(`export const inject = ['commands']
export function apply(ctx) {
  ctx.commands.register({ name: 'wrong-kind', description: 'd', handler: () => ({ kind: 'approve' }) })
  ctx.commands.register({ name: 'empty-submit', description: 'd', handler: () => ({ kind: 'submit', prompt: '' }) })
  ctx.commands.register({ name: 'silent', description: 'd', handler() {} })
}
`)
  t.after(plugin.cleanup)
  const { result } = await activate(host, 'bad-answers', plugin.entry)
  assert.equal(result.status, 'ok')
  await assert.rejects(runCommand(host, 'wrong-kind'), (error) => error.code === -32000 && /unknown result kind/.test(error.message))
  await assert.rejects(runCommand(host, 'empty-submit'), (error) => error.code === -32000 && /submit prompt must be a non-empty string/.test(error.message))
  assert.deepEqual(await runCommand(host, 'silent'), { kind: 'success' })
})

test('plugins cannot provide `commands`, or rewrite the shim every plugin registers through', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const provider = tempPlugin(`export const inject = ['tools']
export function apply(ctx) { ctx.root.provide('commands', { register() {} }) }
`)
  t.after(provider.cleanup)
  const refused = await activate(host, 'commands-provider', provider.entry)
  assert.equal(refused.result.status, 'failed')
  assert.match(refused.result.diagnostic, /may not provide core service `commands`/)

  const hijack = tempPlugin(`export const inject = ['commands']
export function apply(ctx) {
  const proto = Object.getPrototypeOf(ctx.root.commands)
  proto.register = function () { return () => {} }
}
`)
  t.after(hijack.cleanup)
  const { result } = await activate(host, 'commands-hijack', hijack.entry)
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /read ?only|read-only|not extensible|Cannot assign/i)
})

test('a DSH-style command plugin registers through the compatible `commands` shim', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const plugin = tempPlugin(`import { CommandDefinitionId, CommandId } from '@deepseek-ai/dsh-commands/brand'
import assert from 'node:assert/strict'
import * as llm from '@deepseek-ai/dsh-llm'
import * as session from '@deepseek-ai/dsh-session'
import * as prompt from '@deepseek-ai/dsh-system-prompt'
import * as questions from '@deepseek-ai/dsh-user-questions'
export const name = 'dsh-style'
export const inject = ['commands']
export function apply(ctx) {
  assert.deepEqual(Object.keys(llm), ['HarnessError', 'createUserMessage'])
  assert.deepEqual(Object.keys(questions), ['UserQuestionError'])
  const cause = new Error('original cause')
  const failure = new questions.UserQuestionError('human cancelled', 'ASK_CANCELLED', { cause })
  assert.ok(failure instanceof questions.UserQuestionError)
  assert.ok(failure instanceof llm.HarnessError)
  assert.ok(failure instanceof Error)
  assert.equal(failure.name, 'UserQuestionError')
  assert.equal(failure.message, 'human cancelled')
  assert.equal(failure.code, 'ASK_CANCELLED')
  assert.equal(failure.cause, cause)
  const baseFailure = new llm.HarnessError('base', 'FIXTURE', { cause })
  assert.equal(baseFailure.name, 'HarnessError')
  assert.equal(baseFailure.cause, cause)
  assert.deepEqual(Object.keys(session), ['SessionId'])
  assert.deepEqual(Object.keys(prompt), ['PERSONA_PREFIX_SECTION'])
  assert.equal(prompt.PERSONA_PREFIX_SECTION, 'deployment:persona-prefix')
  for (const value of ['', 'session', 7, null, { arbitrary: true }]) assert.equal(session.SessionId(value), value)
  const input = { id: 'forged', role: 'assistant', content: [{ type: 'text', text: 'original' }], source: { kind: 'fixture-review' }, extra: { values: [1] } }
  const message = llm.createUserMessage(input)
  assert.equal(message.role, 'user')
  assert.match(message.id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
  assert.notEqual(message.id, llm.createUserMessage(input).id)
  for (const value of [message, message.content, message.content[0], message.source, message.extra, message.extra.values]) assert.equal(Object.isFrozen(value), true)
  assert.equal(Object.isFrozen(input), false)
  input.content[0].text = 'changed'
  input.extra.values.push(2)
  assert.equal(message.content[0].text, 'original')
  assert.deepEqual(message.extra.values, [1])
  assert.throws(() => { message.source.kind = 'changed' }, TypeError)
  const cyclic = { content: [], source: { kind: 'fixture-review' } }
  cyclic.extra = cyclic
  const cycle = llm.createUserMessage(cyclic)
  assert.equal(cycle.extra.extra, cycle.extra)
  assert.notEqual(cycle.extra, cyclic)
  assert.equal(Object.isFrozen(cycle.extra), true)
  assert.equal(llm.createUserMessage(null).role, 'user')
  assert.equal(llm.createUserMessage({ extra: 7n }).extra, 7n)
  let reads = 0
  const accessor = llm.createUserMessage({ get content() { reads++; return [] }, source: { kind: 'fixture-review' } })
  assert.equal(reads, 1)
  assert.deepEqual(accessor.content, [])
  assert.throws(() => llm.createUserMessage({ content: [() => {}] }), { name: 'DataCloneError' })
  ctx.commands.register({
    definitionId: CommandDefinitionId('dsh-style/echo'),
    name: 'dsh-echo',
    description: 'Echo, DSH style.',
    input: { hint: '<text>' },
    recordInput: false,
    handler: (invocation) => {
      if (invocation.args === 'review') {
        invocation.agent.followup(llm.createUserMessage({ content: [{ type: 'text', text: 'review request' }], source: { kind: 'fixture-review' } }))
        return { kind: 'success', text: 'submitted' }
      }
      return { kind: 'success', text: CommandId(invocation.rawInput.trim()) }
    },
  })
}
`)
  t.after(plugin.cleanup)
  for (const name of ['dsh-llm', 'dsh-session', 'dsh-system-prompt', 'dsh-user-questions']) {
    const root = join(plugin.dir, 'node_modules', '@deepseek-ai', name)
    mkdirSync(root, { recursive: true })
    writeFileSync(join(root, 'package.json'), JSON.stringify({ name: `@deepseek-ai/${name}`, type: 'module', main: 'index.js' }))
    writeFileSync(join(root, 'index.js'), "throw new Error('foreign runtime loaded')\n")
  }
  const { result } = await activate(host, 'dsh-style', plugin.entry)
  assert.deepEqual(result, { status: 'ok', tools: [], commands: ['dsh-echo'] })
  assert.deepEqual(commandNamed(host, 'dsh-echo').spec, { name: 'dsh-echo', description: 'Echo, DSH style.', argument_hint: '<text>' })
  assert.deepEqual(await runCommand(host, 'dsh-echo', 'hi there'), { kind: 'success', text: 'hi there' })
  assert.deepEqual(await host.call('command/run', { handle: commandNamed(host, 'dsh-echo').handle, command_id: 'pure-review', raw_input: 'review', deadline_ms: 5000, session_id: 'pure-session' }), { kind: 'submit', prompt: 'review request', text: 'submitted' })

  // Only the brand helpers are provided; the rest of the package still fails loudly.
  const whole = tempPlugin("import '@deepseek-ai/dsh-commands'\nexport function apply() {}\n")
  t.after(whole.cleanup)
  const refused = await activate(host, 'dsh-whole', whole.entry)
  assert.equal(refused.result.status, 'failed')
  assert.match(refused.result.diagnostic, /requires `@deepseek-ai\/dsh-commands`/)
})

/** Activate several entries of one plugin under one owner, as the core does: one `ext/activate` per entry, in order. */
async function activateEntries(host, name, files) {
  const ref = owner(name)
  const results = []
  for (const file of files) {
    const path = join(FIXTURES, name, file)
    const result = await host.call('ext/activate', { owner: ref, plugin_name: name, entry: { path, sha256: sha256File(path) }, config: {} })
    results.push(result)
    if (result.status !== 'ok') break
  }
  return { ref, results }
}

test('a plugin with two entries activates both under one owner and tears both down together', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const { ref, results } = await activateEntries(host, 'two-entries', ['tools.mjs', 'commands.mjs'])
  assert.deepEqual(results[0], { status: 'ok', tools: ['two_first'], commands: [] })
  // The second answer lists everything the owner has registered so far.
  assert.deepEqual(results[1], { status: 'ok', tools: ['two_first', 'two_second'], commands: ['two-hello'] })
  const registered = host.registry.filter((entry) => entry.op === 'register')
  assert.deepEqual(registered.map((entry) => entry.spec.name), ['two_first', 'two_second', 'two-hello'])
  assert.ok(registered.every((entry) => entry.owner.owner_token === ref.owner_token), 'one owner for every entry')
  const second = registered.find((entry) => entry.spec.name === 'two_second')
  assert.equal((await host.call('tool/call', { handle: second.handle, call_id: 'c1', input: {}, deadline_ms: 5000 })).content[0].text, 'second')

  // Neither entry can be activated twice, and no other plugin can join the owner.
  const again = await host.call('ext/activate', {
    owner: ref,
    plugin_name: 'two-entries',
    entry: { path: join(FIXTURES, 'two-entries', 'tools.mjs'), sha256: sha256File(join(FIXTURES, 'two-entries', 'tools.mjs')) },
    config: {},
  })
  assert.equal(again.status, 'failed')
  assert.match(again.diagnostic, /already activated under this owner/)
  const stranger = await host.call('ext/activate', {
    owner: ref,
    plugin_name: 'someone-else',
    entry: { path: join(FIXTURES, 'two-entries', 'commands.mjs'), sha256: sha256File(join(FIXTURES, 'two-entries', 'commands.mjs')) },
    config: {},
  })
  assert.equal(stranger.status, 'failed')

  // One deactivate disposes the fibers of both entries.
  assert.deepEqual(await host.call('ext/deactivate', { owner: ref }), { disposed: true, leaked: [] })
  for (const entry of registered.filter((r) => r.kind === 'tool')) {
    assert.ok(host.registry.some((e) => e.op === 'unregister' && e.handle === entry.handle), `${entry.spec.name} was unregistered`)
  }
})

test('a failing second entry fails the owner and rolls back the first entry', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const { ref, results } = await activateEntries(host, 'two-entries-failing', ['first.mjs', 'second.mjs'])
  assert.equal(results[0].status, 'ok')
  assert.equal(results[1].status, 'failed')
  assert.match(results[1].diagnostic, /second entry refuses to start/)
  const first = host.registry.find((entry) => entry.op === 'register' && entry.spec.name === 'tef_first')
  assert.ok(first, 'the first entry registered its tool')
  for (let i = 0; i < 40 && !host.registry.some((entry) => entry.op === 'unregister' && entry.handle === first.handle); i++) {
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  assert.ok(host.registry.some((entry) => entry.op === 'unregister' && entry.handle === first.handle), "rollback unregistered the first entry's tool")
  await assert.rejects(host.call('tool/call', { handle: first.handle, call_id: 'c1', input: {}, deadline_ms: 1000 }), (error) => error.code === -32001)
  // The owner is gone: deactivating it is an idempotent success.
  assert.deepEqual(await host.call('ext/deactivate', { owner: ref }), { disposed: true, leaked: [] })
})

test('a plugin is given its config, validated by its own Config schema, and each call carries the workspace and data directory', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const dataDir = mkdtempSync(join(tmpdir(), 'cw-ext-data-'))
  t.after(() => rmSync(dataDir, { recursive: true, force: true }))
  const entry = join(FIXTURES, 'plugin-context', 'index.mjs')

  // The plugin's `Config` schema fills the default for `limit`.
  const { result } = await activate(host, 'plugin-context', entry, { config: { greeting: 'Hi' }, data_dir: dataDir })
  assert.deepEqual(result, { status: 'ok', tools: ['ctx_frozen', 'ctx_note', 'ctx_probe'], commands: ['ctx-probe'] })
  const handleOf = (name) => host.registry.find((e) => e.op === 'register' && e.spec.name === name).handle
  const call = (name, input = {}, extra = {}) =>
    host.call('tool/call', { handle: handleOf(name), call_id: 'c1', input, deadline_ms: 5000, ...extra })

  const probe = await call('ctx_probe', {}, { workspace: '/w/project' })
  assert.deepEqual(probe.structured, {
    config: { greeting: 'Hi', limit: 3 },
    workspace: '/w/project',
    dataDir,
    keys: ['args', 'callId', 'dataDir', 'signal', 'workspace'],
  })
  // The workspace is per call: a call that names none gets none.
  const bare = await call('ctx_probe')
  assert.equal(bare.structured.workspace, null)
  assert.deepEqual(bare.structured.keys, ['args', 'callId', 'dataDir', 'signal'])
  const other = await call('ctx_probe', {}, { workspace: '/w/other' })
  assert.equal(other.structured.workspace, '/w/other')

  // The data directory is the plugin's own to write.
  const note = await call('ctx_note', { text: 'remember' })
  assert.deepEqual(note.structured, { file: join(dataDir, 'note.txt'), text: 'remember' })
  // The context is read-only.
  assert.deepEqual((await call('ctx_frozen')).structured, { changed: false })

  const command = host.registry.find((e) => e.op === 'register' && e.kind === 'command')
  const said = await host.call('command/run', { handle: command.handle, command_id: 'c-1', raw_input: '', deadline_ms: 5000, workspace: '/w/project' })
  assert.deepEqual(JSON.parse(said.text), { config: { greeting: 'Hi', limit: 3 }, workspace: '/w/project', dataDir })

  // A config the plugin's own schema refuses fails the activation, naming the field.
  const bad = await activate(host, 'plugin-context', entry, { config: { limit: 99 }, data_dir: dataDir })
  assert.equal(bad.result.status, 'failed')
  assert.match(bad.result.diagnostic, /limit/)
  // And an activation with no config or data dir (an older core) still works.
  const bare2 = await activate(host, 'plugin-context', entry)
  assert.equal(bare2.result.status, 'ok')
})

// ---- Trust tiers: one host process per tier, told which with `--tier=`.

test('the host refuses to start on an unknown tier, a tier with no value or a tier named twice', async () => {
  for (const args of [['--tier=root'], ['--tier='], ['--tier'], ['--tier=Plugin'], ['--tier=plugin', '--tier=builtin'], ['--tier=builtin', '--tier=builtin']]) {
    const child = spawn(process.execPath, [...HOST_ARGS, BUNDLE, ...args], {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: { ...process.env, ...HOST_ENV },
    })
    let stderr = ''
    let stdoutBytes = 0
    child.stderr.on('data', (chunk) => { stderr += chunk })
    child.stdout.on('data', (chunk) => { stdoutBytes += chunk.length })
    const { code } = await new Promise((resolve) => child.on('exit', (code, signal) => resolve({ code, signal })))
    assert.equal(code, 64, `${args.join(' ')}: ${stderr}`)
    assert.match(stderr, /tier/, args.join(' '))
    assert.equal(stdoutBytes, 0, `${args.join(' ')}: a host that refuses its tier never says hello`)
  }
})

test('a plugin-tier host refuses a host: owner and a builtin-tier host refuses a plugin owner, before loading anything', async (t) => {
  const entry = join(FIXTURES, 'ext-commands', 'index.mjs')
  const request = (ref) => ({ owner: ref, plugin_name: 'ext-commands', entry: { path: entry, sha256: sha256File(entry) }, config: {} })
  const refused = (tier) => (error) => {
    assert.equal(error.code, ErrorCode.InvalidParams)
    assert.match(error.message, new RegExp(`this host serves the ${tier} tier`))
    return true
  }
  // No `--tier=` is the plugin tier, the least-privileged one.
  for (const tier of ['plugin', null]) {
    const host = await startHost({ tier })
    t.after(() => host.stop())
    await assert.rejects(host.call('ext/activate', request(owner('host:ext-commands'))), refused('plugin'))
    assert.equal(host.registry.length, 0, 'nothing of the refused owner was loaded')
    // The same fixture activates under a plugin id.
    const { result } = await activate(host, 'ext-commands')
    assert.equal(result.status, 'ok')
  }

  const builtin = await startHost({ tier: 'builtin' })
  t.after(() => builtin.stop())
  await assert.rejects(builtin.call('ext/activate', request(owner('user/0123456789ab/ext-commands'))), refused('builtin'))
  await assert.rejects(builtin.call('ext/activate', request(owner('ext-commands'))), refused('builtin'))
  assert.equal(builtin.registry.length, 0, 'nothing of the refused owners was loaded')
  // A `host:` owner is what a builtin-tier host takes.
  const ref = owner('host:ext-commands')
  const result = await builtin.call('ext/activate', request(ref))
  assert.equal(result.status, 'ok')
  assert.ok(result.commands.includes('ext-echo'))
  assert.deepEqual(await builtin.call('ext/deactivate', { owner: ref }), { disposed: true, leaked: [] })
})

/** What `dist/builtin-modules.json` says, as `host/hello.builtin_modules` rows. */
function builtinDigests() {
  const manifest = JSON.parse(readFileSync(join(dirname(BUNDLE), 'builtin-modules.json'), 'utf8'))
  return Object.entries(manifest.modules)
    .sort(([a], [b]) => (a < b ? -1 : 1))
    .map(([id, sha256]) => ({ id, sha256 }))
}

test('host/hello says which tier the host serves, and the built-in module digests its build embeds', async (t) => {
  for (const [tier, reported] of [['plugin', 'plugin'], [null, 'plugin'], ['builtin', 'builtin']]) {
    const host = await startHost({ tier })
    t.after(() => host.stop())
    assert.equal(host.hello.tier, reported, `--tier=${tier}`)
    assert.deepEqual(host.hello.builtin_modules, builtinDigests(), `--tier=${tier}`)
  }
})

test('the build records the digest of every built-in module, and only those', () => {
  const dist = join(dirname(BUNDLE))
  const manifest = JSON.parse(readFileSync(join(dist, 'builtin-modules.json'), 'utf8'))
  const built = existsSync(join(dist, 'builtin')) ? readdirSync(join(dist, 'builtin')).sort() : []
  assert.deepEqual(Object.keys(manifest.modules).map((id) => `${id}.mjs`).sort(), built)
  for (const [id, digest] of Object.entries(manifest.modules)) {
    assert.equal(sha256File(join(dist, 'builtin', `${id}.mjs`)), digest, id)
  }
})

// ---- core/call: a tool asking the core to run a core tool for it (`exec.core`).

const coreResult = (text, extra = {}) => ({ content: [{ type: 'text', text }], is_error: false, ...extra })

async function coreCallTool(host, name, input, { ticket = 'ticket-1' } = {}) {
  const { result } = await activate(host, 'core-call')
  assert.equal(result.status, 'ok')
  const registered = host.registry.find((entry) => entry.op === 'register' && entry.spec.name === name)
  return {
    registered,
    call: (callInput, extra = {}) =>
      host.request('tool/call', { handle: registered.handle, call_id: 'cc-1', input: callInput, deadline_ms: 5000, ...(ticket === null ? {} : { ticket }), ...extra }),
  }
}

test('a tool called with a ticket reaches the core through exec.core, and core/call carries its owner, ticket, name and input', async (t) => {
  const host = await startHost({ coreCall: (params) => coreResult(`ran ${params.name}`, { structured: { echoed: params.input } }) })
  t.after(() => host.stop())
  const { call } = await coreCallTool(host, 'cc_call')
  const { promise } = call({ name: 'read', input: { path: 'a.txt' } })
  const out = await promise
  assert.deepEqual(out.structured, { ok: { content: 'ran read', isError: false, structured: { echoed: { path: 'a.txt' } } } })
  assert.equal(host.coreCalls.length, 1)
  const sent = host.coreCalls[0]
  assert.equal(sent.ticket, 'ticket-1')
  assert.equal(sent.name, 'read')
  assert.deepEqual(sent.input, { path: 'a.txt' })
  assert.equal(sent.owner.plugin_id, 'core-call')
  assert.deepEqual(Object.keys(sent).sort(), ['id', 'input', 'name', 'owner', 'ticket'])
})

test('without a ticket exec.core does not exist, and a command invocation never has one', async (t) => {
  const host = await startHost({ coreCall: () => coreResult('x') })
  t.after(() => host.stop())
  const { call } = await coreCallTool(host, 'cc_probe', {}, { ticket: null })
  const bare = (await call({}).promise).structured
  assert.equal(bare.hasCore, false)
  assert.deepEqual(bare.keys, ['args', 'callId', 'signal'])
  const withTicket = (await call({}, { ticket: 'tk' }).promise).structured
  assert.equal(withTicket.hasCore, true)
  assert.deepEqual(withTicket.coreKeys, ['call'])
  const noCore = host.registry.find((entry) => entry.spec.name === 'cc_call')
  assert.deepEqual((await host.request('tool/call', { handle: noCore.handle, call_id: 'c2', input: { name: 'read' }, deadline_ms: 5000 }).promise).structured, { noCore: true })
  const command = host.registry.find((entry) => entry.op === 'register' && entry.kind === 'command')
  const said = await host.call('command/run', { handle: command.handle, command_id: 'c-1', raw_input: '', deadline_ms: 5000 })
  assert.equal(JSON.parse(said.text).hasCore, false)
  assert.equal(host.coreCalls.length, 0, 'nothing reached the core')
})

test('typed refusals reach the tool as CoreCallError codes', async (t) => {
  const errors = {
    refused: { code: ErrorCode.Refused, message: 'policy says no' },
    denied: { code: ErrorCode.Denied, message: 'the user said no' },
    cancelled: { code: ErrorCode.Cancelled, message: 'cancelled' },
    unavailable: { code: ErrorCode.NotAvailable, message: 'turn ended' },
    failed: { code: ErrorCode.Internal, message: 'boom' },
  }
  const host = await startHost({ coreCall: (params) => ({ error: errors[params.name] }) })
  t.after(() => host.stop())
  const { call } = await coreCallTool(host, 'cc_call')
  for (const [name, error] of Object.entries(errors)) {
    const out = (await call({ name }).promise).structured
    assert.deepEqual(out.failed, { name: 'CoreCallError', code: name, message: error.message }, name)
  }
})

test('the host refuses what it must not send: a bad name, input that is not JSON', async (t) => {
  const host = await startHost({ coreCall: () => coreResult('x') })
  t.after(() => host.stop())
  const { call } = await coreCallTool(host, 'cc_local')
  const out = (await call({}).promise).structured
  for (const key of ['emptyName', 'longName', 'notJson', 'cyclic']) {
    assert.equal(out[key].failed.code, 'failed', key)
  }
  assert.equal(host.coreCalls.length, 0)
})

test('cancelling the tool call cancels its pending core/call with $/cancel, and the answer that follows is dropped', async (t) => {
  let release
  const held = new Promise((resolve) => (release = resolve))
  const host = await startHost({ coreCall: () => held })
  t.after(() => host.stop())
  const { call } = await coreCallTool(host, 'cc_call')
  const { id, promise } = call({ name: 'read' })
  await host.waitFor((m) => m.method === 'core/call')
  host.cancel(id)
  await host.waitFor((m) => m.method === '$/cancel')
  assert.deepEqual(host.cancels, [host.coreCalls[0].id])
  await assert.rejects(promise, (error) => error.code === ErrorCode.Cancelled)
  release(coreResult('late'))
  // The host stays usable.
  assert.deepEqual(await host.call('host/ping', {}), {})
})

test('shutdown drops a late core reply after the fake core closes stdin', async () => {
  let release
  const held = new Promise((resolve) => (release = resolve))
  const host = await startHost({ coreCall: () => held })
  const { call } = await coreCallTool(host, 'cc_call')
  call({ name: 'read' })
  await host.waitFor((m) => m.method === 'core/call')

  const stopped = host.stop()
  release(coreResult('late'))
  await stopped
})

test('several core calls can be in flight at once, each answered to its own request', async (t) => {
  const host = await startHost({ coreCall: async (params) => { await new Promise((r) => setTimeout(r, params.input.ms)); return coreResult(String(params.input.ms)) } })
  t.after(() => host.stop())
  const { call } = await coreCallTool(host, 'cc_many')
  // The input is the same for every call, so answers differ only by arrival; all four are answered.
  const out = (await call({ name: 'read', input: { ms: 20 }, count: 4, parallel: true }).promise).structured
  assert.equal(out.outcomes.length, 4)
  assert.ok(out.outcomes.every((o) => o.ok.content === '20'))
  assert.equal(host.coreCalls.length, 4)
  assert.equal(new Set(host.coreCalls.map((c) => c.id)).size, 4)
})

test('storage survives owner reload, refuses disposed owner access, and call identity is per invocation', async (t) => {
  let checkpointResult = true
  let questionResult = { answers: [{ id: 'review', kind: 'choice', label: 'Other', value: 'Other' }] }
  const host = await startHost({
    checkpoint: () => typeof checkpointResult === 'function' ? checkpointResult() : checkpointResult,
    userQuestions: (params, id) => typeof questionResult === 'function' ? questionResult(params, id) : questionResult,
  })
  t.after(() => host.stop())
  const plugin = tempPlugin(`
import { HarnessError } from '@deepseek-ai/dsh-llm'
import { UserQuestionError } from '@deepseek-ai/dsh-user-questions'
export const inject = ['tools', 'commands', 'storage', 'sessions', 'userQuestions']
let savedStorage
let savedAgent
let savedSessions
let savedQuestions
let queuedQuestionOutcome
export async function apply(ctx, config) {
  if (config.capture) { savedStorage = ctx.storage; savedSessions = ctx.sessions; savedQuestions = ctx.userQuestions }
  const identity = (exec) => ({
    frozen: Object.isFrozen(exec),
    sessionId: exec.sessionId ?? null,
    agentId: exec.agentId ?? null,
    originTurnId: exec.originTurnId ?? null,
  })
  const agentIdentity = (agent) => agent === undefined ? null : ({
    id: agent.id,
    cwd: agent.session.header.cwd,
    frozen: [agent, agent.session, agent.session.header].every(Object.isFrozen),
    keys: [Object.keys(agent).sort(), Object.keys(agent.session), Object.keys(agent.session.header)],
    mutationRejected: !Reflect.set(agent, 'id', 'forged') && !Reflect.set(agent.session.header, 'cwd', '/forged'),
  })
  ctx.tools.register({ name: config.tool, description: 'Read owner-local state.', parameters: { type: 'object' },
    async execute(input, exec) {
      if (input.questionProbe) return queuedQuestionOutcome ?? null
      if (input.ask) {
        try {
          const service = input.ask === 'foreign-owner' ? savedQuestions : ctx.userQuestions
          const agent = input.ask === 'forged' ? { id: exec.agent?.id, session: exec.agent?.session } : input.ask === 'retained' ? savedAgent : exec.agent
          const request = { agent, questions: input.questions ?? [{ id: 'review', question: 'Review?', options: [{ label: 'Other' }] }] }
          if (input.optionalUndefined) {
            request.signal = undefined
            request.wait = undefined
            request.questions = request.questions.map(question => ({
              ...question, header: question.header, detail: question.detail, multiSelect: question.multiSelect,
              intent: question.intent === undefined ? undefined : { ...question.intent, callId: undefined },
              options: question.options?.map(option => ({ ...option, description: undefined })),
            }))
          }
          if (input.shape === 'accessor') Object.defineProperty(request.questions[0], 'detail', { enumerable: true, get() { throw new Error('question getter executed') } })
          if (input.shape === 'symbol') request.questions[0][Symbol('hidden')] = 'value'
          if (input.shape === 'nonplain') Object.setPrototypeOf(request.questions[0], { hidden: true })
          if (input.shape === 'sparse') request.questions = new Array(1)
          if (input.shape === 'extra-array-key') request.questions.extra = true
          if (input.shape === 'wait') request.wait = 1
          if (input.shape === 'signal') request.signal = {}
          if (input.shape === 'aborted') request.signal = AbortSignal.abort()
          if (input.ask === 'queued-abort') {
            const checkpoint = ctx.sessions.flush(exec.agent.session)
            const controller = new AbortController()
            const queued = service.ask({ ...request, signal: controller.signal })
            controller.abort()
            queuedQuestionOutcome = await queued.then(value => ({ answered: value }), error => ({ questionError: error.code }))
            await checkpoint
            return queuedQuestionOutcome
          }
          return { asked: input.many ? await Promise.all([service.ask(request), service.ask(request)]) : await service.ask(request) }
        } catch (error) {
          if (typeof error.code === 'string' && (!(error instanceof UserQuestionError) || !(error instanceof HarnessError) || error.name !== 'UserQuestionError')) throw new Error('lost question error singleton')
          return { questionError: error.code ?? 'untyped' }
        }
      }
      if (input.flush) {
        try {
          const target = input.flush === 'forged' ? { header: { cwd: exec.workspace } } : input.flush === 'retained' ? savedAgent?.session : exec.agent?.session
          const service = input.flush === 'foreign-owner' ? savedSessions : ctx.sessions
          return { flushed: input.many ? await Promise.all([service.flush(target), service.flush(target)]) : await service.flush(target) }
        } catch (error) { return { flushError: error.code } }
      }
      if (input.captureAgent) savedAgent = exec.agent
      if (input.readSavedAgent) return { previous: agentIdentity(savedAgent), current: agentIdentity(exec.agent) }
      if (input.saved) { try { await savedStorage.get('state'); return { accessed: true } } catch (error) { return { error: error.code } } }
      if ('set' in input) await ctx.storage.set('state', input.set)
      return { value: (await ctx.storage.get('state')) ?? null, ...identity(exec), agent: agentIdentity(exec.agent) }
    },
  })
  ctx.commands.register({ name: config.command, description: 'Inspect invocation identity.',
    handler: (exec) => JSON.stringify(identity(exec)),
  })
}
`)
  t.after(plugin.cleanup)
  const firstDir = join(plugin.dir, 'first-state')
  const otherDir = join(plugin.dir, 'other-state')
  mkdirSync(firstDir)
  mkdirSync(otherDir)
  const firstConfig = { tool: 'first_state', command: 'first-state', capture: true }
  const first = await activate(host, 'first-state-owner', plugin.entry, { config: firstConfig, data_dir: firstDir })
  assert.equal(first.result.status, 'ok')
  const handle = (name) => host.registry.findLast((entry) => entry.op === 'register' && entry.spec.name === name).handle
  const tool = (name, input = {}, extra = {}) => host.call('tool/call', { handle: handle(name), call_id: 'state-call', input, deadline_ms: 5000, ...extra })
  const identity = { session_id: 'session-one', agent_id: 'agent-one', origin_turn_id: 'turn-one', workspace: '/workspace/one' }
  const projection = (id, cwd) => ({ id, cwd, frozen: true, keys: [['id', 'session'], ['header'], ['cwd']], mutationRejected: true })
  assert.deepEqual((await tool('first_state', { set: { count: 1 }, captureAgent: true }, identity)).structured,
    { value: { count: 1 }, frozen: true, sessionId: 'session-one', agentId: 'agent-one', originTurnId: 'turn-one', agent: projection('agent-one', '/workspace/one') })
  assert.deepEqual((await tool('first_state')).structured,
    { value: { count: 1 }, frozen: true, sessionId: null, agentId: null, originTurnId: null, agent: null })
  const rootIdentity = { session_id: 'session-two', origin_turn_id: 'turn-two', workspace: '/workspace/two' }
  assert.deepEqual((await tool('first_state', { readSavedAgent: true }, rootIdentity)).structured,
    { previous: projection('agent-one', '/workspace/one'), current: projection('session-two', '/workspace/two') })
  assert.equal((await tool('first_state', {}, { session_id: 'session-only' })).structured.agent, null)
  assert.equal((await tool('first_state', {}, { workspace: '/workspace/only' })).structured.agent, null)
  assert.equal((await tool('first_state', {}, { ...identity, session_id: '' })).structured.agent, null)
  assert.equal((await tool('first_state', {}, { ...identity, workspace: '' })).structured.agent, null)
  const checkpointIdentity = { ...rootIdentity, checkpoint_ticket: 'exact-checkpoint-ticket' }
  assert.deepEqual((await tool('first_state', { flush: 'current' }, rootIdentity)).structured, { flushError: ErrorCode.NotAvailable })
  assert.deepEqual((await tool('first_state', { flush: 'forged' }, checkpointIdentity)).structured, { flushError: ErrorCode.NotAvailable })
  assert.deepEqual((await tool('first_state', { flush: 'retained' }, checkpointIdentity)).structured, { flushError: ErrorCode.NotAvailable })
  assert.equal(host.checkpoints.length, 0)
  assert.deepEqual((await tool('first_state', { flush: 'current' }, checkpointIdentity)).structured, { flushed: true })
  assert.deepEqual(host.checkpoints[0], { id: host.checkpoints[0].id, owner: first.ref, ticket: 'exact-checkpoint-ticket' })
  checkpointResult = false
  assert.deepEqual((await tool('first_state', { flush: 'current' }, checkpointIdentity)).structured, { flushed: false })
  checkpointResult = 'not-a-receipt'
  assert.deepEqual((await tool('first_state', { flush: 'current' }, checkpointIdentity)).structured, { flushError: ErrorCode.Internal })
  checkpointResult = { error: { code: ErrorCode.Refused, message: 'checkpoint refused' } }
  assert.deepEqual((await tool('first_state', { flush: 'current' }, checkpointIdentity)).structured, { flushError: ErrorCode.Refused })
  let inFlight = 0
  let peak = 0
  checkpointResult = async () => {
    inFlight++
    peak = Math.max(peak, inFlight)
    await new Promise(resolve => setTimeout(resolve, 10))
    inFlight--
    return true
  }
  assert.deepEqual((await tool('first_state', { flush: 'current', many: true }, checkpointIdentity)).structured, { flushed: [true, true] })
  assert.equal(peak, 1, 'one invocation serializes its checkpoint requests')
  let releaseCheckpoint
  checkpointResult = new Promise(resolve => { releaseCheckpoint = resolve })
  const pendingCheckpoint = host.request('tool/call', {
    handle: handle('first_state'), call_id: 'cancel-checkpoint', input: { flush: 'current' }, deadline_ms: 5000,
    ...rootIdentity, checkpoint_ticket: 'cancel-checkpoint-ticket',
  })
  await host.waitFor(message => message.method === 'session/checkpoint' && message.params.ticket === 'cancel-checkpoint-ticket')
  const checkpointRequest = host.checkpoints.find(request => request.ticket === 'cancel-checkpoint-ticket')
  const cancelledCheckpoint = host.waitFor(message => message.method === '$/cancel' && message.params.id === checkpointRequest.id)
  host.cancel(pendingCheckpoint.id)
  await assert.rejects(pendingCheckpoint.promise, error => error.code === ErrorCode.Cancelled)
  await cancelledCheckpoint
  releaseCheckpoint(true)
  checkpointResult = true
  assert.deepEqual(await host.call('host/ping', {}), {})
  const questionIdentity = { ...rootIdentity, question_ticket: 'exact-question-ticket' }
  for (const [input, extra, code] of [
    [{ ask: 'current' }, rootIdentity, ErrorCode.NotAvailable],
    [{ ask: 'current' }, checkpointIdentity, ErrorCode.NotAvailable],
    [{ ask: 'current' }, identity, 'DELEGATED_CALLER'],
    [{ ask: 'current' }, { question_ticket: 'missing-context-ticket' }, ErrorCode.NotAvailable],
    [{ ask: 'forged' }, questionIdentity, 'CALLER_NOT_LIVE'],
    [{ ask: 'retained' }, questionIdentity, 'CALLER_NOT_LIVE'],
  ]) assert.deepEqual((await tool('first_state', input, extra)).structured, { questionError: code })
  assert.equal(host.questions.length, 0)
  for (const shape of ['accessor', 'symbol', 'nonplain', 'sparse', 'extra-array-key', 'signal']) {
    assert.deepEqual((await tool('first_state', { ask: 'current', shape }, questionIdentity)).structured, { questionError: ErrorCode.InvalidParams }, shape)
  }
  assert.deepEqual((await tool('first_state', { ask: 'current', questions: [] }, questionIdentity)).structured, { questionError: 'EMPTY_QUESTIONS' })
  assert.deepEqual((await tool('first_state', { ask: 'current', shape: 'wait' }, questionIdentity)).structured, { questionError: ErrorCode.NotAvailable })
  assert.deepEqual((await tool('first_state', { ask: 'current', shape: 'aborted' }, questionIdentity)).structured, { questionError: 'ASK_ABORTED' })
  assert.equal(host.questions.length, 0)
  for (const question of [
    { id: 'plan', question: 'Proceed?', detail: 'Plan', options: [{ label: 'yes' }], intent: { kind: 'plan-review', approve: 'missing' } },
    { id: 'plan', question: 'Proceed?', options: [{ label: 'yes' }], intent: { kind: 'plan-review', approve: 'yes' } },
  ]) assert.deepEqual((await tool('first_state', { ask: 'current', questions: [question] }, questionIdentity)).structured, { questionError: 'BAD_INTENT' })
  assert.equal(host.questions.length, 0)
  const selectedOther = { answers: [{ id: 'review', selected: ['Other'] }] }
  assert.deepEqual((await tool('first_state', { ask: 'current', optionalUndefined: true }, questionIdentity)).structured, { asked: selectedOther })
  assert.deepEqual(host.questions[0], {
    id: host.questions[0].id, owner: first.ref, ticket: 'exact-question-ticket',
    questions: [{ id: 'review', question: 'Review?', header: '', options: [{ label: 'Other', description: '' }], allow_free_text: true, multi_select: false }],
  })
  questionResult = { answers: [{ id: 'free', kind: 'custom', label: '', value: 'Other, literally typed' }] }
  for (const options of [undefined, []]) {
    assert.deepEqual((await tool('first_state', { ask: 'current', optionalUndefined: true, questions: [{ id: 'free', question: 'Explain', ...(options === undefined ? {} : { options }) }] }, questionIdentity)).structured,
      { asked: { answers: [{ id: 'free', selected: [], custom: 'Other, literally typed' }] } })
    assert.deepEqual(host.questions.at(-1).questions[0].options, [])
  }
  const labels = Array.from({ length: 100 }, (_, index) => index === 0 ? 'Other' : index === 99 ? 'one,two' : 'choice-' + index)
  questionResult = { answers: [{ id: 'many', kind: 'choice', label: 'one,two', value: 'one,two' }] }
  assert.deepEqual((await tool('first_state', { ask: 'current', optionalUndefined: true, questions: [{ id: 'many', question: 'Choose', options: labels.map(label => ({ label })) }] }, questionIdentity)).structured,
    { asked: { answers: [{ id: 'many', selected: ['one,two'] }] } })
  assert.deepEqual(host.questions.at(-1).questions[0].options, labels.map(label => ({ label, description: '' })))
  questionResult = { answers: [{ id: 'plan', kind: 'choice', label: 'Approve', value: 'Approve' }] }
  assert.deepEqual((await tool('first_state', { ask: 'current', optionalUndefined: true, questions: [{ id: 'plan', question: 'Proceed?', detail: 'Reviewed plan', options: [{ label: 'Approve' }], intent: { kind: 'plan-review', approve: 'Approve' } }] }, questionIdentity)).structured,
    { asked: { answers: [{ id: 'plan', selected: ['Approve'] }] } })
  assert.deepEqual(host.questions.at(-1).questions[0].intent, { kind: 'plan-review', approve: 'Approve' })
  const mixedQuestions = [
    { id: 'multi', question: 'Choose several', multiSelect: true, detail: 'Exact labels', options: [{ label: 'Other' }, { label: 'a,b' }] },
    { id: 'skip', question: 'Optional', options: [] },
  ]
  questionResult = { answers: [
    { id: 'skip', kind: 'skipped', label: '', value: '' },
    { id: 'multi', kind: 'choice', label: 'Other', value: 'Other' },
    { id: 'multi', kind: 'choice', label: 'a,b', value: 'a,b' },
    { id: 'multi', kind: 'custom', label: '', value: 'my, own text' },
  ] }
  assert.deepEqual((await tool('first_state', { ask: 'current', questions: mixedQuestions }, questionIdentity)).structured,
    { asked: { answers: [{ id: 'multi', selected: ['Other', 'a,b'], custom: 'my, own text' }, { id: 'skip', selected: [] }] } })
  assert.equal(host.questions.at(-1).questions[0].detail, 'Exact labels')
  assert.equal(host.questions.at(-1).questions[0].multi_select, true)
  questionResult = { answers: [{ id: 'multi', kind: 'choice', label: 'Other', value: 'Other' }] }
  assert.deepEqual((await tool('first_state', { ask: 'current', questions: mixedQuestions }, questionIdentity)).structured, { questionError: ErrorCode.Internal })
  const choice = { id: 'review', kind: 'choice', label: 'Other', value: 'Other' }
  for (const receipt of [
    null, {}, { answers: null }, { answers: [] }, { answers: [null] },
    { answers: [{ ...choice, id: 'foreign' }] }, { answers: [{ ...choice, kind: 'unknown' }] },
    { answers: [{ id: 'review', label: 'Other', value: 'Other' }] },
    { answers: [{ ...choice, value: 'Other, something' }] }, { answers: [choice, choice] },
    { answers: [{ ...choice, label: 'not offered', value: 'not offered' }] },
    { answers: [{ id: 'review', kind: 'custom', label: 'Other', value: 'custom' }] },
    { answers: [{ id: 'review', kind: 'custom', label: '', value: ' ' }] },
    { answers: [choice, { id: 'review', kind: 'custom', label: '', value: 'custom' }] },
    { answers: [{ id: 'review', kind: 'skipped', label: '', value: '' }, choice] },
    { answers: [{ id: 'review', kind: 'skipped', label: '', value: 'not empty' }] },
  ]) {
    questionResult = receipt
    assert.deepEqual((await tool('first_state', { ask: 'current' }, questionIdentity)).structured, { questionError: ErrorCode.Internal }, JSON.stringify(receipt))
  }
  questionResult = { error: { code: ErrorCode.Refused, message: 'questions refused' } }
  assert.deepEqual((await tool('first_state', { ask: 'current' }, questionIdentity)).structured, { questionError: ErrorCode.Refused })
  for (const [code, data, expected] of [
    [ErrorCode.NotAvailable, { user_question_error: 'NO_PROVIDER' }, 'NO_PROVIDER'],
    [ErrorCode.NotAvailable, undefined, ErrorCode.NotAvailable],
    [ErrorCode.NotAvailable, { user_question_error: 'unknown' }, ErrorCode.NotAvailable],
    [ErrorCode.Refused, { user_question_error: 'NO_PROVIDER' }, ErrorCode.Refused],
    [ErrorCode.Cancelled, undefined, ErrorCode.Cancelled],
    [ErrorCode.Cancelled, { user_question_error: 'ASK_CANCELLED' }, 'ASK_CANCELLED'],
    [ErrorCode.NotAvailable, { user_question_error: 'ASK_CANCELLED' }, ErrorCode.NotAvailable],
    [ErrorCode.Cancelled, { user_question_error: 'NO_PROVIDER' }, ErrorCode.Cancelled],
  ]) {
    questionResult = { error: { code, message: 'answerer result', ...(data === undefined ? {} : { data }) } }
    assert.deepEqual((await tool('first_state', { ask: 'current' }, questionIdentity)).structured, { questionError: expected })
  }
  inFlight = 0
  peak = 0
  questionResult = async () => {
    inFlight++
    peak = Math.max(peak, inFlight)
    await new Promise(resolve => setTimeout(resolve, 10))
    inFlight--
    return { answers: [choice] }
  }
  assert.deepEqual((await tool('first_state', { ask: 'current', many: true }, questionIdentity)).structured, { asked: [selectedOther, selectedOther] })
  assert.equal(peak, 1, 'one invocation serializes its questions')
  const beforeQueued = host.questions.length
  checkpointResult = new Promise(resolve => { releaseCheckpoint = resolve })
  const queuedQuestion = host.request('tool/call', {
    handle: handle('first_state'), call_id: 'queued-question', input: { ask: 'queued-abort' }, deadline_ms: 5000,
    ...questionIdentity, checkpoint_ticket: 'question-queue-checkpoint-ticket',
  })
  await host.waitFor(message => message.method === 'session/checkpoint' && message.params.ticket === 'question-queue-checkpoint-ticket')
  assert.deepEqual((await tool('first_state', { questionProbe: true })).structured, { questionError: 'ASK_ABORTED' })
  assert.equal(host.questions.length, beforeQueued, 'a cancelled queued question never reaches core')
  releaseCheckpoint(true)
  assert.deepEqual((await queuedQuestion.promise).structured, { questionError: 'ASK_ABORTED' })
  checkpointResult = true
  let releaseQuestion
  questionResult = new Promise(resolve => { releaseQuestion = resolve })
  const pendingQuestion = host.request('tool/call', {
    handle: handle('first_state'), call_id: 'cancel-question', input: { ask: 'current' }, deadline_ms: 5000,
    ...rootIdentity, question_ticket: 'cancel-question-ticket',
  })
  await host.waitFor(message => message.method === 'user-questions/ask' && message.params.ticket === 'cancel-question-ticket')
  const questionRequest = host.questions.find(request => request.ticket === 'cancel-question-ticket')
  const cancelledQuestion = host.waitFor(message => message.method === '$/cancel' && message.params.id === questionRequest.id)
  host.cancel(pendingQuestion.id)
  await assert.rejects(pendingQuestion.promise, error => error.code === ErrorCode.Cancelled)
  await cancelledQuestion
  host.send({ jsonrpc: '2.0', id: questionRequest.id, result: { answers: [choice] } })
  releaseQuestion({ answers: [choice] })
  questionResult = { answers: [choice] }
  assert.deepEqual(await host.call('host/ping', {}), {})
  assert.deepEqual((await tool('first_state', { ask: 'current' }, questionIdentity)).structured, { asked: selectedOther })
  assert.equal(host.coreCalls.length, 0, 'read-only projections do not grant core calls')
  const command = await host.call('command/run', { handle: handle('first-state'), command_id: 'state-command', raw_input: '', deadline_ms: 5000, ...identity })
  assert.deepEqual(JSON.parse(command.text), { frozen: true, sessionId: 'session-one', agentId: 'agent-one', originTurnId: 'turn-one' })
  const other = await activate(host, 'other-state-owner', plugin.entry, { config: { tool: 'other_state', command: 'other-state' }, data_dir: otherDir })
  assert.equal(other.result.status, 'ok')
  const beforeForeign = host.questions.length
  assert.deepEqual((await tool('other_state', { ask: 'foreign-owner' }, questionIdentity)).structured, { questionError: ErrorCode.NotAvailable })
  assert.equal(host.questions.length, beforeForeign)
  questionResult = new Promise(resolve => { releaseQuestion = resolve })
  const disposedQuestion = host.request('tool/call', {
    handle: handle('first_state'), call_id: 'disposed-question', input: { ask: 'current' }, deadline_ms: 5000,
    ...rootIdentity, question_ticket: 'disposed-question-ticket',
  })
  await host.waitFor(message => message.method === 'user-questions/ask' && message.params.ticket === 'disposed-question-ticket')
  await host.call('ext/deactivate', { owner: first.ref })
  releaseQuestion({ answers: [choice] })
  assert.deepEqual((await disposedQuestion.promise).structured, { questionError: ErrorCode.Cancelled })
  questionResult = { answers: [choice] }
  assert.deepEqual((await tool('other_state', { ask: 'foreign-owner' }, questionIdentity)).structured, { questionError: ErrorCode.NotAvailable })
  assert.deepEqual((await tool('other_state', { flush: 'foreign-owner' }, checkpointIdentity)).structured, { flushError: ErrorCode.NotAvailable })
  assert.deepEqual((await tool('other_state', { saved: true })).structured, { error: 'not_available' })
  assert.equal((await tool('other_state')).structured.value, null, 'another owner directory has separate state')
  assert.deepEqual((await tool('other_state', { readSavedAgent: true }, rootIdentity)).structured,
    { previous: projection('agent-one', '/workspace/one'), current: projection('session-two', '/workspace/two') }, 'a retained projection stays inert after owner disposal')
  const reloaded = await activate(host, 'reloaded-state-owner', plugin.entry, { config: { ...firstConfig, capture: false }, data_dir: firstDir })
  assert.equal(reloaded.result.status, 'ok')
  assert.deepEqual((await tool('first_state')).structured.value, { count: 1 }, 'the assigned owner directory preserves state on reload')
})

test('programmable pre-execute listeners propose deny, ask, rewrite and context without core authority', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const { ref, result } = await activate(host, 'hook-policy')
  assert.equal(result.status, 'ok')
  const hook = host.registry.find((entry) => entry.op === 'register' && entry.kind === 'hook')
  assert.equal(hook.spec.name, 'tools/pre-execute')
  const evaluate = (path) => host.call('hook/evaluate', {
    handle: hook.handle, event: 'tools/pre-execute', deadline_ms: 5000,
    payload: { name: 'read', call_id: 'hook-1', input: { path }, mode: 'Agent', workspace: '/workspace', model: 'fixture' },
  })
  assert.deepEqual(await evaluate('before.txt'), { kind: 'revise', input: { path: 'after.txt' } })
  assert.deepEqual(await evaluate('blocked.txt'), { kind: 'deny', reason: 'blocked by fixture' })
  assert.deepEqual(await evaluate('ask.txt'), { kind: 'ask', reason: 'fixture asks' })
  assert.deepEqual(await evaluate('context.txt'), { kind: 'annotate', text: 'fixture context' })
  assert.deepEqual(await evaluate('allow.txt'), { kind: 'abstain' })
  assert.deepEqual(await evaluate('allow.txt'), { kind: 'abstain' })
  assert.equal(host.logs.filter((log) => log.msg.includes('allow is an abstention')).length, 1)
  assert.deepEqual(await evaluate('other.txt'), { kind: 'abstain' })
  await assert.rejects(evaluate('malformed.txt'), /revise needs a JSON object/)
  await assert.rejects(evaluate('throw.txt'), /fixture hook failure/)
  const pending = host.request('hook/evaluate', {
    handle: hook.handle, event: 'tools/pre-execute', deadline_ms: 5000,
    payload: { name: 'read', call_id: 'hook-held', input: { path: 'held.txt' }, mode: 'Agent', workspace: '/workspace', model: 'fixture' },
  })
  host.cancel(pending.id)
  await assert.rejects(pending.promise, (error) => error.code === ErrorCode.Cancelled)
  assert.deepEqual(await host.call('ext/deactivate', { owner: ref }), { disposed: true, leaked: [] })
  await assert.rejects(evaluate('before.txt'), (error) => error.code === ErrorCode.NotAvailable)
})
