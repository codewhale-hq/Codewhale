import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'
import { activate, startHost } from './harness.mjs'

const here = dirname(fileURLToPath(import.meta.url))
let remoteAdapter
let Context
let ErrorCode

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-remote-'))
  const bundle = async (entry, name) => {
    const outfile = join(dir, name)
    await build({ entryPoints: [join(here, entry)], outfile, bundle: true, platform: 'node', format: 'esm', packages: 'external', absWorkingDir: join(here, '..'), logLevel: 'silent' })
    return import(pathToFileURL(outfile).href)
  }
  ;({ remoteAdapter } = await bundle('../src/dsh/remote.ts', 'remote.mjs'))
  ;({ ErrorCode } = await bundle('../src/protocol.ts', 'protocol.mjs'))
  ;({ Context } = await import('@deepseek-ai/cordis'))
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

async function harness() {
  const calls = []
  const host = {
    tier: 'plugin',
    rpc: { request: async () => { throw new Error('remote never calls the core directly') }, notify() {} },
    ownerOf: () => undefined,
    liveInvocation: () => undefined,
    broker: async (ctx, service, method, input) => { calls.push([service, method, input]); return { ok: method } },
    log() {},
  }
  const ctx = new Context()
  ctx.plugin(remoteAdapter.define(host))
  await settle()
  return { ctx, calls }
}

test('remote sends every method through the remote broker with a typed payload and nothing else', async () => {
  const { ctx, calls } = await harness()
  assert.deepEqual(await ctx.remote.status(), { ok: 'status' })
  await ctx.remote.pair()
  await ctx.remote.pair({ label: 'phone' })
  await ctx.remote.clients()
  await ctx.remote.revoke('cwrg_1')
  assert.deepEqual(calls, [
    ['remote', 'status', {}],
    ['remote', 'pair', {}],
    ['remote', 'pair', { label: 'phone' }],
    ['remote', 'clients', {}],
    ['remote', 'revoke', { grant: 'cwrg_1' }],
  ])
})

test('remote refuses malformed arguments locally, before any broker call', async () => {
  const { ctx, calls } = await harness()
  assert.throws(() => ctx.remote.pair({ label: 3 }), (error) => error.code === ErrorCode.InvalidParams)
  for (const id of [undefined, '', 7, 'x'.repeat(129)]) {
    assert.throws(() => ctx.remote.revoke(id), (error) => error.code === ErrorCode.InvalidParams)
  }
  assert.equal(typeof ctx.remote.listen, 'undefined')
  assert.deepEqual(calls, [])
})

test('a plugin may inject `remote` and may never provide it, in the shared scope or an isolated one', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const plugin = (source) => {
    const dir = mkdtempSync(join(tmpdir(), 'cw-ext-host-remote-'))
    t.after(() => rmSync(dir, { recursive: true, force: true }))
    const entry = join(dir, 'index.mjs')
    writeFileSync(entry, source)
    return entry
  }
  const fake = `{ status() { return 'fake' }, pair() { return 'fake' }, clients() { return 'fake' }, revoke() { return 'fake' } }`
  const providers = {
    'remote-root-provider': `ctx.root.provide('remote', ${fake})`,
    'remote-own-provider': `ctx.provide('remote', ${fake})`,
    'remote-isolated-provider': `ctx.isolate('remote').provide('remote', ${fake})`,
  }
  for (const [name, body] of Object.entries(providers)) {
    const { result } = await activate(host, name, plugin(`export const inject = ['tools']\nexport function apply(ctx) { ${body} }\n`))
    assert.equal(result.status, 'failed', name)
    assert.match(result.diagnostic, /may not provide core service `remote`/, name)
  }
  const user = await activate(host, 'remote-user', plugin(`export const inject = ['remote']
export function apply(ctx) {
  for (const method of ['status', 'pair', 'clients', 'revoke']) if (typeof ctx.remote[method] !== 'function') throw new Error('ctx.remote.' + method + ' is missing')
  if (ctx.remote.constructor.name !== 'RemoteService') throw new Error('ctx.remote is not the host service')
}
`))
  assert.equal(user.result.status, 'ok', user.result.diagnostic)
})
