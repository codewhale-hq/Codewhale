import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'
import { startHost, activate, owner as makeOwner, sha256File } from './harness.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const creatorBundle = join(here, '..', 'dist', 'builtin', 'creator.mjs')
const upstreamToolsPath = join(here, '..', 'src', 'dsh', 'upstream', 'creator', 'plugin-manager', 'tools.js')
const upstreamEscalationPath = join(here, '..', 'src', 'dsh', 'upstream', 'creator', 'sandbox', 'escalation.js')
const licensesPath = join(here, '..', 'dist', 'LICENSES.txt')
let HostRoot

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-creator-test-'))
  const rootOut = join(dir, 'root.mjs')
  await build({
    entryPoints: [join(here, '../src/root.ts')],
    outfile: rootOut,
    bundle: true,
    platform: 'node',
    format: 'esm',
    packages: 'external',
    absWorkingDir: join(here, '..'),
    logLevel: 'silent',
  })
  ;({ HostRoot } = await import(pathToFileURL(rootOut).href))
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

function sha256Text(text) {
  return createHash('sha256').update(text, 'utf8').digest('hex')
}

function fakeRpc(brokerFn) {
  let seq = 1
  return {
    notify: () => {},
    request: async (method, params) => {
      if (method === 'registry/register') return { handle: seq++ }
      if (method === 'registry/unregister') return {}
      if (method === 'dsh/broker') return brokerFn(params)
      return {}
    },
  }
}

function makeTempPlugin(t, code) {
  const dir = mkdtempSync(join(tmpdir(), 'cw-creator-unit-'))
  t.after(() => rmSync(dir, { recursive: true, force: true }))
  const path = join(dir, 'index.mjs')
  writeFileSync(path, code)
  return { path, sha256: sha256File(path) }
}

function makeCallParams(handle, id, input, extra = {}) {
  return {
    handle,
    call_id: id,
    input,
    deadline_ms: 5000,
    workspace: '/tmp/ws',
    caller_tools: [],
    broker_tickets: [{ service: 'plugins', ticket: 't1' }],
    ...extra,
  }
}

test('the creator module registers plugin_manager with DSH\'s own description and parameters', async (t) => {
  const host = await startHost({ tier: 'builtin' })
  t.after(() => host.stop())

  const { result } = await activate(host, 'host:creator', creatorBundle)
  assert.equal(result.status, 'ok')
  const registered = host.registry.find((r) => r.op === 'register' && r.kind === 'tool' && r.spec.name === 'plugin_manager')
  assert.ok(registered)

  const upstreamSource = readFileSync(upstreamToolsPath, 'utf8')
  const match = upstreamSource.match(/name:\s*'plugin_manager',\s*\n\s*description:\s*'([^']+)'/)
  assert.ok(match, 'expected pinned plugin_manager description in upstream tools.js')
  assert.equal(sha256Text(registered.spec.description), sha256Text(match[1]))

  assert.deepEqual(registered.spec.input_schema.required, ['action'])
  assert.deepEqual(Object.keys(registered.spec.input_schema.properties), [
    'action',
    'target',
    'enabled',
    'runtimeVersion',
    'acceptRisk',
    'approvedBuilds',
    'registry',
    'offset',
    'limit',
  ])
  assert.deepEqual(registered.spec.input_schema.properties.action.enum, [
    'list_plugins',
    'list_bundles',
    'set_plugin',
    'set_bundle',
    'install_bundle',
    'remove_bundle',
    'list_version_exemptions',
    'set_version_exemption',
  ])
})

test('list_bundles sends exactly one dsh/broker frame for service plugins, method list', async (t) => {
  const host = await startHost({
    tier: 'builtin',
    dshBroker: (params) => {
      assert.equal(params.service, 'plugins')
      if (params.method === 'posture') return { mode: 'workspace-write' }
      if (params.method === 'list') return { entries: [{ name: 'demo-bundle', rows: [{ id: 'demo-row', enabled: true }], updatable: true, meta: {} }] }
      throw new Error(`unexpected method ${params.method}`)
    },
  })
  t.after(() => host.stop())

  await activate(host, 'host:creator', creatorBundle)
  const registered = host.registry.find((r) => r.op === 'register' && r.spec.name === 'plugin_manager')
  const res = await host.call('tool/call', makeCallParams(registered.handle, 'pm-list-1', { action: 'list_bundles' }, { session_id: 's1' }))
  assert.deepEqual(JSON.parse(res.content[0].text), {
    entries: [{ name: 'demo-bundle', rows: [{ id: 'demo-row', enabled: true }] }],
    total: 1,
    nextOffset: null,
  })
  const listCalls = host.brokerCalls.filter((c) => c.service === 'plugins' && c.method === 'list')
  assert.equal(listCalls.length, 1)
  assert.deepEqual(listCalls[0].input, {})
})

test('the approval step makes one posture read, asks nobody, and runs before the manager call', async (t) => {
  const order = []
  const asks = []
  const host = await startHost({
    tier: 'builtin',
    userQuestionsAsk: (params) => {
      asks.push(params)
      return { answers: {} }
    },
    dshBroker: (params) => {
      order.push(`${params.service}:${params.method}`)
      if (params.method === 'posture') return { mode: 'workspace-write' }
      if (params.method === 'enable') return { changed: true, application: 'applied' }
      throw new Error(`unexpected method ${params.method}`)
    },
  })
  t.after(() => host.stop())

  await activate(host, 'host:creator', creatorBundle)
  const registered = host.registry.find((r) => r.op === 'register' && r.spec.name === 'plugin_manager')
  const res = await host.call('tool/call', makeCallParams(registered.handle, 'pm-order-1', { action: 'set_bundle', target: 'b1', enabled: true }, { session_id: 's1' }))
  assert.equal(JSON.parse(res.content[0].text).changed, true)
  assert.deepEqual(order, ['plugins:posture', 'plugins:enable'])
  assert.equal(asks.length, 0)
})

test('a read-only posture surfaces DSH\'s own rejected sentence and the manager is never called', async (t) => {
  const upstreamEscalation = readFileSync(upstreamEscalationPath, 'utf8')
  const rejectedMatch = upstreamEscalation.includes('the user rejected escalating this ${subject} to "${mode}"; it stays denied, so stop and explain instead of working around it')
  assert.ok(rejectedMatch)

  const host = await startHost({
    tier: 'builtin',
    dshBroker: (params) => {
      if (params.method === 'posture') return { mode: 'read-only' }
      throw new Error(`manager method ${params.method} should never be called`)
    },
  })
  t.after(() => host.stop())

  await activate(host, 'host:creator', creatorBundle)
  const registered = host.registry.find((r) => r.op === 'register' && r.spec.name === 'plugin_manager')
  await assert.rejects(
    host.call('tool/call', makeCallParams(registered.handle, 'pm-ro-1', { action: 'list_bundles' }, { session_id: 's1' })),
    (err) => err.message.includes('the user rejected escalating this plugin management operation to "danger-full-access"; it stays denied, so stop and explain instead of working around it'),
  )
  assert.deepEqual(host.brokerCalls.map((c) => c.method), ['posture'])
})

test('a broker refusal or error answers unavailable and the manager is never called', async (t) => {
  let behavior = 'refusal'
  const host = await startHost({
    tier: 'builtin',
    dshBroker: (params) => {
      if (params.method === 'posture') {
        if (behavior === 'refusal') return { error: { code: -32002, message: 'ticket refused', data: { dsh_refusal: 'ticket_refused' } } }
        return { error: { code: -32603, message: 'internal failure' } }
      }
      throw new Error(`manager method ${params.method} should never be called`)
    },
  })
  t.after(() => host.stop())

  await activate(host, 'host:creator', creatorBundle)
  const registered = host.registry.find((r) => r.op === 'register' && r.spec.name === 'plugin_manager')
  await assert.rejects(
    host.call('tool/call', makeCallParams(registered.handle, 'pm-unavail-1', { action: 'list_bundles' }, { session_id: 's1' })),
    /sandbox escalation to "danger-full-access" requires approval, but no approval channel is available/,
  )
  behavior = 'internal'
  await assert.rejects(
    host.call('tool/call', makeCallParams(registered.handle, 'pm-unavail-2', { action: 'list_plugins' }, { session_id: 's1' })),
    /sandbox escalation to "danger-full-access" requires approval, but no approval channel is available/,
  )
  assert.deepEqual(host.brokerCalls.map((c) => c.method), ['posture', 'posture'])

  const unitRoot = new HostRoot(fakeRpc(async () => { throw new Error('boom') }), 'builtin')
  const unitFile = makeTempPlugin(t, `
export const inject = ['tools']
export function apply(ctx) {
  ctx.tools.register({
    name: 'probe_unavail',
    parameters: { type: 'object', properties: {} },
    async execute(_args, exec) { return await ctx.get('approval').request({ agent: exec.agent, callId: exec.callId }) },
  })
}
`)
  const ownerRef = makeOwner('host:creator')
  await unitRoot.activate({ owner: ownerRef, plugin_name: 'creator', entry: unitFile, config: {} })
  const handle = unitRoot.owners.get(ownerRef.owner_token).tools.values().next().value.handle
  const res = await unitRoot.callTool(handle, {}, 'c1', new AbortController().signal, '/tmp/ws', undefined, { session_id: 's1', broker_tickets: [] })
  assert.equal(res.structured, 'unavailable')
})

test('sandboxPolicy never reports danger-full-access', async (t) => {
  const root = new HostRoot(fakeRpc(async (params) => {
    if (params.method === 'posture') return { mode: 'danger-full-access' }
    return {}
  }), 'builtin')
  const unitFile = makeTempPlugin(t, `
export const inject = ['tools', 'sandboxPolicy']
export function apply(ctx) {
  ctx.tools.register({
    name: 'probe_policy',
    parameters: { type: 'object', properties: {} },
    async execute(_args, exec) {
      return {
        plain: ctx.sandboxPolicy.resolve({}),
        session: ctx.sandboxPolicy.resolve({ session: exec.agent.session }),
        spoofed: ctx.sandboxPolicy.resolve({ mode: 'danger-full-access' }),
        approval: await ctx.get('approval').request({ agent: exec.agent, callId: exec.callId }),
      }
    },
  })
}
`)
  const ownerRef = makeOwner('host:creator')
  assert.equal((await root.activate({ owner: ownerRef, plugin_name: 'creator', entry: unitFile, config: {} })).status, 'ok')
  const handle = root.owners.get(ownerRef.owner_token).tools.values().next().value.handle
  const res = await root.callTool(handle, {}, 'c-pol', new AbortController().signal, '/tmp/ws', undefined, { session_id: 's1', broker_tickets: [{ service: 'plugins', ticket: 't1' }] })
  assert.deepEqual(res.structured.plain, { mode: 'read-only' })
  assert.deepEqual(res.structured.session, { mode: 'read-only' })
  assert.deepEqual(res.structured.spoofed, { mode: 'read-only' })
  assert.notEqual(res.structured.plain.mode, 'danger-full-access')
  assert.equal(res.structured.approval, 'allowed-once')
})

test('list_version_exemptions answers the target version with no broker call', async (t) => {
  const host = await startHost({
    tier: 'builtin',
    dshBroker: (params) => {
      if (params.method === 'posture') return { mode: 'workspace-write' }
      throw new Error(`unexpected manager broker call ${params.method}`)
    },
  })
  t.after(() => host.stop())

  await activate(host, 'host:creator', creatorBundle)
  const registered = host.registry.find((r) => r.op === 'register' && r.spec.name === 'plugin_manager')
  const exRes = await host.call('tool/call', makeCallParams(registered.handle, 'pm-ex-1', { action: 'list_version_exemptions' }, { session_id: 's1' }))
  assert.deepEqual(JSON.parse(exRes.content[0].text), {
    runtimeVersion: '0.2.2-rc.1',
    exemptions: [],
    warnings: ['Codewhale does not exempt plugins from the host compatibility contract.'],
  })
  assert.deepEqual(host.brokerCalls.map((c) => c.method), ['posture'])

  let brokerCalls = 0
  const root = new HostRoot(fakeRpc(async () => { brokerCalls++; return {} }), 'builtin')
  const unitFile = makeTempPlugin(t, `
export const inject = ['tools', 'pluginManager']
export function apply(ctx) {
  ctx.tools.register({
    name: 'probe_ex',
    parameters: { type: 'object', properties: {} },
    async execute() { return ctx.pluginManager.listVersionExemptions() },
  })
}
`)
  const ownerRef = makeOwner('host:creator')
  await root.activate({ owner: ownerRef, plugin_name: 'creator', entry: unitFile, config: {} })
  const handle = root.owners.get(ownerRef.owner_token).tools.values().next().value.handle
  const res = await root.callTool(handle, {}, 'c-ex', new AbortController().signal, '/tmp/ws', undefined, { session_id: 's1', broker_tickets: [] })
  assert.deepEqual(res.structured, {
    exemptions: [],
    warnings: ['Codewhale does not exempt plugins from the host compatibility contract.'],
  })
  assert.equal(brokerCalls, 0)
})

test('outside a live invocation every adapter method refuses', async (t) => {
  const calls = []
  const root = new HostRoot(fakeRpc(async (params) => { calls.push(params); return { mode: 'workspace-write' } }), 'builtin')
  let captured
  const unitFile = makeTempPlugin(t, `
export const inject = ['tools', 'pluginManager', 'sandboxPolicy']
export function apply(ctx) {
  globalThis.__cwCreatorCap = ctx
  ctx.tools.register({
    name: 'probe_spoof',
    parameters: { type: 'object', properties: { mode: { type: 'string' } } },
    async execute(args, exec) {
      if (args.mode === 'spoof-call') return await ctx.get('approval').request({ agent: exec.agent, callId: 'wrong-call' })
      if (args.mode === 'spoof-agent') return await ctx.get('approval').request({ agent: { ...exec.agent }, callId: exec.callId })
      return null
    },
  })
}
`)
  const ownerRef = makeOwner('host:creator')
  await root.activate({ owner: ownerRef, plugin_name: 'creator', entry: unitFile, config: {} })
  captured = globalThis.__cwCreatorCap
  delete globalThis.__cwCreatorCap

  const pm = captured.pluginManager
  const sp = captured.sandboxPolicy
  const ap = captured.get('approval')

  assert.throws(() => sp.resolve({}), /requires a live tool invocation/)
  assert.throws(() => pm.listVersionExemptions(), /requires a live tool invocation/)
  for (const invoke of [
    () => pm.listBundles(),
    () => pm.listPlugins(),
    () => pm.setPluginEnabled('p1', true),
    () => pm.setBundleEnabled('b1', true),
    () => pm.installBundle('pkg@1.0.0'),
    () => pm.removeBundle('pkg'),
    () => pm.setVersionExemption('pkg@1.0.0', '0.2.2-rc.1', true, true),
    () => pm.cancelInstall('req-1'),
  ]) {
    await assert.rejects(invoke, /requires a live tool invocation/)
  }
  assert.equal(await ap.request({ agent: { id: 's1' }, callId: 'c1' }), 'unavailable')
  assert.equal(calls.length, 0)

  const handle = root.owners.get(ownerRef.owner_token).tools.values().next().value.handle
  const identity = { session_id: 's1', broker_tickets: [{ service: 'plugins', ticket: 't1' }] }
  const spoofCall = await root.callTool(handle, { mode: 'spoof-call' }, 'c-s1', new AbortController().signal, '/tmp/ws', undefined, identity)
  assert.equal(spoofCall.structured, 'unavailable')
  const spoofAgent = await root.callTool(handle, { mode: 'spoof-agent' }, 'c-s2', new AbortController().signal, '/tmp/ws', undefined, identity)
  assert.equal(spoofAgent.structured, 'unavailable')
  assert.equal(calls.length, 0)

  const otherFile = makeTempPlugin(t, `
export const inject = ['tools']
export function apply(ctx) {
  ctx.tools.register({
    name: 'other_probe',
    parameters: { type: 'object', properties: {} },
    async execute() {
      const errors = []
      for (const fn of [
        () => ctx.get('sandboxPolicy').resolve({}),
        () => ctx.get('pluginManager').listVersionExemptions(),
      ]) {
        try { fn(); errors.push('unexpected-ok') } catch (err) { errors.push(err.message) }
      }
      for (const fn of [
        () => ctx.get('pluginManager').listBundles(),
        () => ctx.get('pluginManager').listPlugins(),
        () => ctx.get('pluginManager').setPluginEnabled('p1', false),
        () => ctx.get('pluginManager').setBundleEnabled('b1', true),
        () => ctx.get('pluginManager').installBundle('pkg@1.0.0'),
        () => ctx.get('pluginManager').removeBundle('pkg'),
        () => ctx.get('pluginManager').setVersionExemption('pkg@1.0.0', '0.2.2-rc.1', true, true),
        () => ctx.get('pluginManager').cancelInstall('req-1'),
      ]) {
        try { await fn(); errors.push('unexpected-ok') } catch (err) { errors.push(err.message) }
      }
      return { errors, approval: await ctx.get('approval').request({}) }
    },
  })
}
`)
  const otherRef = makeOwner('host:other')
  await root.activate({ owner: otherRef, plugin_name: 'other', entry: otherFile, config: {} })
  const otherHandle = root.owners.get(otherRef.owner_token).tools.values().next().value.handle
  const otherRes = await root.callTool(otherHandle, {}, 'c-other', new AbortController().signal, '/tmp/ws', undefined, identity)
  assert.equal(otherRes.structured.approval, 'unavailable')
  assert.equal(otherRes.structured.errors.length, 10)
  for (const msg of otherRes.structured.errors) assert.match(msg, /creator/i)
  assert.equal(calls.length, 0)
})

test('an invocation with no agent fails with DSH\'s own sentence and asks and calls nothing', async (t) => {
  const upstreamEscalation = readFileSync(upstreamEscalationPath, 'utf8')
  assert.ok(upstreamEscalation.includes('sandbox escalation to "${mode}" requires approval, but the call has no agent to route it through'))

  const asks = []
  const host = await startHost({
    tier: 'builtin',
    userQuestionsAsk: (params) => { asks.push(params); return { answers: {} } },
    dshBroker: () => { throw new Error('broker should not be called without an agent') },
  })
  t.after(() => host.stop())

  await activate(host, 'host:creator', creatorBundle)
  const registered = host.registry.find((r) => r.op === 'register' && r.spec.name === 'plugin_manager')
  await assert.rejects(
    host.call('tool/call', makeCallParams(registered.handle, 'pm-no-agent', { action: 'list_bundles' })),
    (err) => err.message.includes('sandbox escalation to "danger-full-access" requires approval, but the call has no agent to route it through'),
  )
  assert.equal(host.brokerCalls.length, 0)
  assert.equal(asks.length, 0)
})

test('an aborted call answers cancelled and the manager is never called', async (t) => {
  const calls = []
  const root = new HostRoot(fakeRpc(async (params) => { calls.push(params); return { mode: 'workspace-write' } }), 'builtin')
  let managerCalls = 0
  const unitFile = makeTempPlugin(t, `
export const inject = ['tools', 'pluginManager']
export function apply(ctx) {
  ctx.tools.register({
    name: 'probe_abort',
    parameters: { type: 'object', properties: { nonAborted: { type: 'boolean' } } },
    async execute(args, exec) {
      const controller = new AbortController()
      if (!args?.nonAborted) controller.abort()
      const decision = await ctx.get('approval').request({ agent: exec.agent, callId: exec.callId, signal: controller.signal })
      if (decision === 'allowed-once') {
        globalThis.__cwManagerCalled++
        await ctx.pluginManager.listBundles()
      }
      return decision
    },
  })
}
`)
  globalThis.__cwManagerCalled = 0
  const ownerRef = makeOwner('host:creator')
  await root.activate({ owner: ownerRef, plugin_name: 'creator', entry: unitFile, config: {} })
  const handle = root.owners.get(ownerRef.owner_token).tools.values().next().value.handle
  const res = await root.callTool(handle, {}, 'c-abort', new AbortController().signal, '/tmp/ws', undefined, { session_id: 's1', broker_tickets: [{ service: 'plugins', ticket: 't1' }] })
  assert.equal(res.structured, 'cancelled')
  assert.equal(globalThis.__cwManagerCalled, 0)
  assert.equal(calls.length, 0)

  const midFlightAbort = new AbortController()
  const cancelCalls = []
  const cancelRoot = new HostRoot(fakeRpc(async (params) => {
    cancelCalls.push(params.method)
    midFlightAbort.abort()
    throw new Error('aborted mid-flight')
  }), 'builtin')
  await cancelRoot.activate({ owner: ownerRef, plugin_name: 'creator', entry: unitFile, config: {} })
  const cancelHandle = cancelRoot.owners.get(ownerRef.owner_token).tools.values().next().value.handle
  const rpcCancelRes = await cancelRoot.callTool(cancelHandle, { nonAborted: true }, 'c-abort-rpc', midFlightAbort.signal, '/tmp/ws', undefined, { session_id: 's1', broker_tickets: [{ service: 'plugins', ticket: 't1' }] }).catch((err) => ({ aborted: err.message }))
  if (!rpcCancelRes.aborted) assert.equal(rpcCancelRes.structured, 'cancelled')
  assert.equal(globalThis.__cwManagerCalled, 0)
  assert.deepEqual(cancelCalls, ['posture'])
  delete globalThis.__cwManagerCalled

  const creatorRoot = new HostRoot(fakeRpc(async () => { throw new Error('broker must not be called when already aborted') }), 'builtin')
  const creatorRef = makeOwner('host:creator')
  const creatorAct = await creatorRoot.activate({ owner: creatorRef, plugin_name: 'creator', entry: { path: creatorBundle, sha256: sha256File(creatorBundle) }, config: {} })
  assert.equal(creatorAct.status, 'ok')
  const pmHandle = creatorRoot.owners.get(creatorRef.owner_token).tools.values().next().value.handle
  const alreadyAborted = new AbortController()
  alreadyAborted.abort()
  await assert.rejects(
    creatorRoot.callTool(pmHandle, { action: 'list_bundles' }, 'pm-abort-unit', alreadyAborted.signal, '/tmp/ws', undefined, { session_id: 's1', broker_tickets: [{ service: 'plugins', ticket: 't1' }] }),
    /cancelled/,
  )
  assert.equal(calls.length, 0)
})

test('a plugin-tier host refuses a plugin that injects pluginManager, sandboxPolicy or approval, or provides sandboxPolicy or approval', async (t) => {
  const calls = []
  const pluginRoot = new HostRoot(fakeRpc(async (params) => { calls.push(params); return {} }), 'plugin')

  for (const service of ['sandboxPolicy', 'approval']) {
    const file = makeTempPlugin(t, `export const inject = ['${service}']; export function apply() {}`)
    const res = await pluginRoot.activate({ owner: makeOwner(`bad-inject-${service}`), plugin_name: `bad-${service}`, entry: file, config: {} })
    assert.equal(res.status, 'failed', service)
    assert.match(res.diagnostic, new RegExp(`${service}.*belongs to the Codewhale core`))
  }

  for (const service of ['sandboxPolicy', 'approval', 'pluginManager']) {
    const file = makeTempPlugin(t, `export function apply(ctx) { ctx.root.provide('${service}', {}) }`)
    const res = await pluginRoot.activate({ owner: makeOwner(`bad-provide-${service}`), plugin_name: `bad-p-${service}`, entry: file, config: {} })
    assert.equal(res.status, 'failed', service)
    assert.match(res.diagnostic, new RegExp(`extension may not provide core service \`${service}\``))
  }

  const catalogPlugin = makeTempPlugin(t, `
export const inject = ['tools', 'pluginManager']
export function apply(ctx) {
  ctx.tools.register({
    name: 'catalog_probe',
    parameters: { type: 'object', properties: {} },
    async execute() { return await ctx.pluginManager.removeBundle('demo') },
  })
}
`)
  const okOwner = makeOwner('catalog-plugin')
  const okRes = await pluginRoot.activate({ owner: okOwner, plugin_name: 'catalog-probe', entry: catalogPlugin, config: {} })
  assert.equal(okRes.status, 'ok')
  const handle = pluginRoot.owners.get(okOwner.owner_token).tools.values().next().value.handle
  const out = await pluginRoot.callTool(handle, {}, 'c-cat', new AbortController().signal, '/tmp/ws', undefined, { session_id: 's1', broker_tickets: [] })
  assert.equal(out.structured.error.code, 'management-required')
  assert.equal(calls.length, 0)
})

test('a built-in module other than creator that injects pluginManager, sandboxPolicy or approval fails activation', async (t) => {
  const builtinRoot = new HostRoot(fakeRpc(async () => ({})), 'builtin')
  for (const service of ['pluginManager', 'sandboxPolicy', 'approval']) {
    const file = makeTempPlugin(t, `export const inject = ['${service}']; export function apply() {}`)
    const res = await builtinRoot.activate({ owner: makeOwner(`host:other-${service}`), plugin_name: `other-${service}`, entry: file, config: {} })
    assert.equal(res.status, 'failed', service)
    assert.match(res.diagnostic, new RegExp(`${service}.*belongs to the Codewhale core`))
  }
})

test('the built-in module registers no tool but plugin_manager', async (t) => {
  const host = await startHost({ tier: 'builtin' })
  t.after(() => host.stop())

  const { result } = await activate(host, 'host:creator', creatorBundle)
  assert.equal(result.status, 'ok')
  assert.deepEqual(result.tools, ['plugin_manager'])
  assert.deepEqual(result.commands, [])
  const registrations = host.registry.filter((entry) => entry.op === 'register')
  assert.equal(registrations.length, 1)
  assert.equal(registrations[0].kind, 'tool')
  assert.equal(registrations[0].spec.name, 'plugin_manager')
})

test('LICENSES.txt credits dsh-plugin-manager and dsh-sandbox', () => {
  const notices = readFileSync(licensesPath, 'utf8')
  assert.match(notices, /@deepseek-ai\/dsh-plugin-manager@0\.2\.2-rc\.1 \(MIT\)/)
  assert.match(notices, /@deepseek-ai\/dsh-sandbox@0\.2\.2-rc\.1 \(MIT\)/)
})
