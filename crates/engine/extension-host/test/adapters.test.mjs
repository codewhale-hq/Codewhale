import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'

const here = dirname(fileURLToPath(import.meta.url))
const rpc = { request: async () => ({}), notify: () => {}, tier: 'plugin' }
let HostRoot
let Service
let DSH_ADAPTERS

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-adapters-'))
  const outfile = join(dir, 'root.mjs')
  await build({
    entryPoints: [join(here, '../src/root.ts')],
    outfile,
    bundle: true,
    platform: 'node',
    format: 'esm',
    packages: 'external',
    absWorkingDir: join(here, '..'),
    logLevel: 'silent',
  })
  const adapters = join(dir, 'adapters.mjs')
  await build({
    entryPoints: [join(here, '../src/dsh/adapters.ts')],
    outfile: adapters,
    bundle: true,
    platform: 'node',
    format: 'esm',
    packages: 'external',
    absWorkingDir: join(here, '..'),
    logLevel: 'silent',
  })
  ;({ HostRoot } = await import(pathToFileURL(outfile).href))
  ;({ DSH_ADAPTERS } = await import(pathToFileURL(adapters).href))
  ;({ Service } = await import('@deepseek-ai/cordis'))
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

const adapterFor = (service) => ({ service, define: () => class extends Service { constructor(ctx) { super(ctx, service) } } })

test('the shipped adapter table holds the reviewed adapters, and the root still builds', () => {
  assert.deepEqual(DSH_ADAPTERS.map((adapter) => adapter.service), ['sessions', 'systemPrompt', 'subagents', 'agents', 'remote'])
  assert.ok(new HostRoot(rpc, 'plugin'))
  assert.ok(new HostRoot(rpc, 'builtin'))
})

test('an adapter for a service compatibility.json does not declare is refused at construction', () => {
  assert.throws(() => new HostRoot(rpc, 'plugin', [...DSH_ADAPTERS, adapterFor('llm')]), /llm is installed but not declared in compatibility\.json/)
})

test('an adapter may not replace a service the root already provides', () => {
  assert.throws(() => new HostRoot(rpc, 'plugin', [adapterFor('storage')]), /duplicates a service the host root already provides/)
  assert.throws(() => new HostRoot(rpc, 'plugin', [...DSH_ADAPTERS, adapterFor('sessions')]), /duplicates a service/)
})

test('the adapter broker refuses locally, without sending, when no live invocation holds a ticket for the service', async () => {
  const sent = []
  const peer = { ...rpc, request: async (method, params) => { sent.push([method, params]); return {} } }
  let host
  const capture = { service: 'llm', define: (adapterHost) => { host = adapterHost; return adapterFor('llm').define() } }
  assert.throws(() => new HostRoot(peer, 'plugin', [capture]), /not declared in compatibility\.json/)
  for (const service of ['workspace', 'llm', 'vault']) {
    await assert.rejects(host.broker({}, service, 'read', {}), (error) => error.code === -32001 && /broker is only available to a live tool invocation/.test(error.message))
  }
  assert.deepEqual(sent, [])
})
