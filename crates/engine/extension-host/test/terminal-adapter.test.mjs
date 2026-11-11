import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'

const here = dirname(fileURLToPath(import.meta.url))
let terminalAdapter
let Context
let ErrorCode

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-terminal-'))
  const bundle = async (entry, name) => {
    const outfile = join(dir, name)
    await build({ entryPoints: [join(here, entry)], outfile, bundle: true, platform: 'node', format: 'esm', packages: 'external', absWorkingDir: join(here, '..'), logLevel: 'silent' })
    return import(pathToFileURL(outfile).href)
  }
  ;({ terminalAdapter } = await bundle('../src/dsh/terminal.ts', 'terminal.mjs'))
  ;({ ErrorCode } = await bundle('../src/protocol.ts', 'protocol.mjs'))
  ;({ Context } = await import('@deepseek-ai/cordis'))
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

async function harness() {
  const calls = []
  const host = {
    tier: 'plugin',
    rpc: { request: async () => { throw new Error('terminal never calls the core directly') }, notify() {} },
    ownerOf: () => undefined,
    liveInvocation: () => undefined,
    broker: async (ctx, service, method, input) => { calls.push([service, method, input]); return { ok: method } },
    log() {},
  }
  const ctx = new Context()
  ctx.plugin(terminalAdapter.define(host))
  await settle()
  return { ctx, calls }
}

test('terminal sends every method through the terminal broker with a typed payload and nothing else', async () => {
  const { ctx, calls } = await harness()
  assert.deepEqual(await ctx.terminal.list(), { ok: 'list' })
  await ctx.terminal.run('make')
  await ctx.terminal.run('make', { session: 'build', timeoutSecs: 30, cwd: '/', sandbox_permissions: 'danger' })
  await ctx.terminal.send('build', 'y\n', { waitMs: 0 })
  await ctx.terminal.wait('build')
  await ctx.terminal.wait('build', { timeoutSecs: 600 })
  await ctx.terminal.cancel('build')
  await ctx.terminal.reset('build')
  assert.deepEqual(calls, [
    ['terminal', 'list', {}],
    ['terminal', 'run', { command: 'make' }],
    ['terminal', 'run', { command: 'make', session: 'build', timeout_secs: 30 }],
    ['terminal', 'send', { session: 'build', text: 'y\n', wait_ms: 0 }],
    ['terminal', 'wait', { session: 'build' }],
    ['terminal', 'wait', { session: 'build', timeout_secs: 600 }],
    ['terminal', 'cancel', { session: 'build' }],
    ['terminal', 'reset', { session: 'build' }],
  ])
})

test('terminal refuses malformed arguments locally, before any broker call', async () => {
  const { ctx, calls } = await harness()
  const refused = (act) => assert.throws(act, (error) => error.code === ErrorCode.InvalidParams)
  for (const command of [undefined, '', 7, ['ls']]) refused(() => ctx.terminal.run(command))
  for (const name of [undefined, '', 7, '../term-1', 'dsh:b:x', 'a b', 'n'.repeat(65)]) {
    refused(() => ctx.terminal.run('true', { session: name === undefined ? null : name }))
    refused(() => ctx.terminal.send(name, 'x'))
    refused(() => ctx.terminal.wait(name))
    refused(() => ctx.terminal.cancel(name))
    refused(() => ctx.terminal.reset(name))
  }
  refused(() => ctx.terminal.send('build', ''))
  for (const secs of [0, 601, 1.5, '30']) {
    refused(() => ctx.terminal.run('true', { timeoutSecs: secs }))
    refused(() => ctx.terminal.wait('build', { timeoutSecs: secs }))
  }
  refused(() => ctx.terminal.send('build', 'x', { waitMs: 60_001 }))
  for (const absent of ['spawn', 'kill', 'resize', 'read', 'exec']) assert.equal(typeof ctx.terminal[absent], 'undefined')
  assert.deepEqual(calls, [])
})
