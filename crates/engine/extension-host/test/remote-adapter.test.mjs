import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'

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
