import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'

const here = dirname(fileURLToPath(import.meta.url))
let workspaceAdapter
let Context
let ErrorCode

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-workspace-'))
  const bundle = async (entry, name) => {
    const outfile = join(dir, name)
    await build({ entryPoints: [join(here, entry)], outfile, bundle: true, platform: 'node', format: 'esm', packages: 'external', absWorkingDir: join(here, '..'), logLevel: 'silent' })
    return import(pathToFileURL(outfile).href)
  }
  ;({ workspaceAdapter } = await bundle('../src/dsh/workspace.ts', 'workspace.mjs'))
  ;({ ErrorCode } = await bundle('../src/protocol.ts', 'protocol.mjs'))
  ;({ Context } = await import('@deepseek-ai/cordis'))
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

async function harness() {
  const calls = []
  const host = {
    tier: 'plugin',
    rpc: { request: async () => { throw new Error('workspace never calls the core directly') }, notify() {} },
    ownerOf: () => undefined,
    liveInvocation: () => undefined,
    broker: async (ctx, service, method, input) => { calls.push([service, method, input]); return { ok: method } },
    log() {},
  }
  const ctx = new Context()
  ctx.plugin(workspaceAdapter.define(host))
  await settle()
  return { ctx, calls }
}

test('workspace sends every method through the workspace broker with a typed payload and nothing else', async () => {
  const { ctx, calls } = await harness()
  assert.deepEqual(await ctx.workspace.read('src/a.ts'), { ok: 'read' })
  await ctx.workspace.read('src/a.ts', { startLine: 3, maxLines: 10, follow: true })
  await ctx.workspace.list()
  await ctx.workspace.list('src')
  await ctx.workspace.write('src/b.ts', 'x', { expectedHash: 'h', mode: 0o777 })
  await ctx.workspace.write('src/c.ts', '')
  await ctx.workspace.exec(['git', 'rev-parse', '--show-toplevel'], { cwd: '/w', env: { GIT_DIR: '/' }, shell: true })
  assert.deepEqual(calls, [
    ['workspace', 'read', { path: 'src/a.ts' }],
    ['workspace', 'read', { path: 'src/a.ts', start_line: 3, max_lines: 10 }],
    ['workspace', 'list', {}],
    ['workspace', 'list', { path: 'src' }],
    ['workspace', 'write', { path: 'src/b.ts', content: 'x', expected_hash: 'h' }],
    ['workspace', 'write', { path: 'src/c.ts', content: '' }],
    ['workspace', 'exec', { argv: ['git', 'rev-parse', '--show-toplevel'], cwd: '/w' }],
  ])
})

test('workspace refuses malformed arguments locally, before any broker call', async () => {
  const { ctx, calls } = await harness()
  const refused = (act) => assert.throws(act, (error) => error.code === ErrorCode.InvalidParams)
  for (const bad of [undefined, '', 7, ' a', 'a\nb', 'p'.repeat(513), ['a']]) {
    refused(() => ctx.workspace.read(bad))
    refused(() => ctx.workspace.write(bad, 'x'))
    if (bad !== undefined) refused(() => ctx.workspace.list(bad))
  }
  for (const lines of [0, 10_001, 1.5, '3']) refused(() => ctx.workspace.read('a', { maxLines: lines }))
  refused(() => ctx.workspace.read('a', { startLine: 0 }))
  for (const content of [undefined, 7, 'c'.repeat(1024 * 1024 + 1)]) refused(() => ctx.workspace.write('a', content))
  refused(() => ctx.workspace.write('a', 'x', { expectedHash: '' }))
  for (const command of [undefined, 'git status', [], [7], ['git', 'a\u0000b'], new Array(10).fill('git')]) refused(() => ctx.workspace.exec(command))
  refused(() => ctx.workspace.exec(['git'], { cwd: '' }))
  for (const absent of ['delete', 'rename', 'spawn', 'shell', 'watch', 'readSync']) assert.equal(typeof ctx.workspace[absent], 'undefined')
  assert.deepEqual(calls, [])
})
