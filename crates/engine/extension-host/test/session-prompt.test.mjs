import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'

const here = dirname(fileURLToPath(import.meta.url))
let sessionsAdapter
let systemPromptAdapter
let Context
let ErrorCode

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-session-prompt-'))
  const bundle = async (entry, name) => {
    const outfile = join(dir, name)
    await build({ entryPoints: [join(here, entry)], outfile, bundle: true, platform: 'node', format: 'esm', packages: 'external', absWorkingDir: join(here, '..'), logLevel: 'silent' })
    return import(pathToFileURL(outfile).href)
  }
  ;({ sessionsAdapter, systemPromptAdapter } = await bundle('../src/dsh/session-prompt.ts', 'session-prompt.mjs'))
  ;({ ErrorCode } = await bundle('../src/protocol.ts', 'protocol.mjs'))
  ;({ Context } = await import('@deepseek-ai/cordis'))
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

function flushHarness(invocation, requests) {
  const session = {}
  const host = {
    tier: 'plugin',
    rpc: { request: async (method, params) => { requests.push({ method, params }); return true }, notify() {} },
    ownerOf: () => invocation?.owner,
    liveInvocation: () => invocation,
    log() {},
  }
  const ctx = new Context()
  ctx.plugin(sessionsAdapter.define(host))
  return { ctx, session }
}

const live = (extra = {}) => {
  const session = {}
  return { owner: { ref: { plugin_id: 'p' } }, session, ticket: 'ticket', signal: new AbortController().signal, pending: Promise.resolve(), ...extra }
}

test('sessions.flush refuses a live invocation that carries no checkpoint ticket', async () => {
  const requests = []
  const invocation = live({ ticket: undefined })
  const { ctx } = flushHarness(invocation, requests)
  await settle()
  await assert.rejects(ctx.sessions.flush(invocation.session), (error) => error.code === ErrorCode.NotAvailable)
  assert.equal(requests.length, 0)
})

test('sessions.flush refuses a session other than the invocation session and sends nothing', async () => {
  const requests = []
  const invocation = live()
  const { ctx } = flushHarness(invocation, requests)
  await settle()
  await assert.rejects(ctx.sessions.flush({}), (error) => error.code === ErrorCode.NotAvailable)
  await assert.rejects(ctx.sessions.flush(undefined), (error) => error.code === ErrorCode.NotAvailable)
  assert.equal(requests.length, 0)
})

test('sessions.flush sends the exact owner and ticket, and a stale invocation is refused before the receipt returns', async () => {
  const requests = []
  const invocation = live()
  let current = invocation
  const host = { tier: 'plugin', rpc: { request: async (method, params) => { requests.push({ method, params }); current = undefined; return true }, notify() {} }, ownerOf: () => invocation.owner, liveInvocation: () => current, log() {} }
  const ctx = new Context()
  ctx.plugin(sessionsAdapter.define(host))
  await settle()
  await assert.rejects(ctx.sessions.flush(invocation.session), (error) => error.code === ErrorCode.NotAvailable)
  assert.deepEqual(requests, [{ method: 'session/checkpoint', params: { owner: invocation.owner.ref, ticket: 'ticket' } }])
})

test('systemPrompt stays child-only: the root context refuses sections', async () => {
  const ctx = new Context()
  ctx.plugin(systemPromptAdapter.define({}))
  await settle()
  assert.throws(() => ctx.systemPrompt.section({ name: 'x', text: 'y' }), (error) => error.code === ErrorCode.NotAvailable)
  assert.throws(() => ctx.systemPrompt.getSectionOrder('X'), (error) => error.code === ErrorCode.NotAvailable)
})
