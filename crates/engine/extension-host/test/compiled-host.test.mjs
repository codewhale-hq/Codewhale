// Actual compiled image, same HostRoot/protocol/services, fake Engine authority.
// CI must name the image explicitly; normal source suites record a visible skip.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync, spawn } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { activate, BUNDLE, sha256File, startHost } from './harness.mjs'
import { ErrorCode } from '../dist/protocol.mjs'

const binary = process.env.CODEWHALE_COMPILED_HOST_TEST_BINARY
if (binary && !existsSync(binary)) throw new Error(`compiled image does not exist: ${binary}`)
const compiledTest = (name, run) => test(name, { skip: !binary && 'compiled image not requested' }, run)
const plugin = (source) => {
  const dir = mkdtempSync(join(tmpdir(), 'cw-compiled-host-'))
  const entry = join(dir, 'index.mjs')
  writeFileSync(entry, source)
  return { dir, entry, cleanup: () => rmSync(dir, { recursive: true, force: true }) }
}
const start = (options = {}) => startHost({ ...options, compiledHost: binary })

compiledTest('compiled identity and both trust tiers match canonical source', async (t) => {
  const info = JSON.parse(execFileSync(binary, ['--codewhale-host-info'], { encoding: 'utf8', env: { ...process.env, BUN_OPTIONS: '', BUN_BE_BUN: '0' } }))
  assert.equal(info.kind, 'codewhale-extension-host')
  assert.equal(info.bundle_sha256, sha256File(BUNDLE))
  for (const tier of ['plugin', 'builtin']) {
    const host = await start({ tier })
    try {
      assert.equal(host.hello.bundle_sha256, info.bundle_sha256)
      assert.deepEqual(host.hello.runtime, { name: 'bun', version: info.version })
      assert.equal(host.hello.tier, tier)
      assert.ok(host.readyMs <= 1500, `ready ${host.readyMs}ms exceeds existing 1500ms gate`)
      if (process.platform !== 'win32') {
        const rss = Number(execFileSync('ps', ['-o', 'rss=', '-p', String(host.child.pid)], { encoding: 'utf8' }).trim()) / 1024
        assert.ok(rss <= 160, `idle ${rss}MiB exceeds existing 160MiB gate`)
        t.diagnostic(`${tier}: ready ${host.readyMs.toFixed(1)}ms; RSS ${rss.toFixed(1)}MiB`)
      }
    } finally { await host.stop() }
  }
})

compiledTest('compiled Native modules share services and cancellation withdraws owner', async (t) => {
  const fixture = plugin(`import { Context } from '@deepseek-ai/cordis'
export const inject = ['tools']
export function apply(ctx) {
  if (!Context.is(ctx)) throw new Error('foreign Cordis')
  ctx.tools.register({ name: 'held', description: '', parameters: { type: 'object', properties: {} }, execute(_input, call) {
    return new Promise((resolve) => call.signal.addEventListener('abort', () => resolve('aborted'), { once: true }))
  } })
}`)
  const host = await start()
  t.after(async () => { await host.stop(); fixture.cleanup() })
  const { ref, result } = await activate(host, 'compiled-owner', fixture.entry)
  assert.equal(result.status, 'ok', result.diagnostic)
  const handle = host.registry.find((entry) => entry.op === 'register').handle
  const call = host.request('tool/call', { handle, call_id: 'held', input: {}, deadline_ms: 5000 })
  setTimeout(() => host.cancel(call.id), 50)
  await assert.rejects(call.promise, (error) => error.code === ErrorCode.Cancelled)
  await host.call('ext/deactivate', { owner: ref, reason: 'test disposal' })
  assert.ok(host.registry.some((entry) => entry.op === 'unregister' && entry.handle === handle))
  await assert.rejects(host.call('tool/call', { handle, call_id: 'retired', input: {}, deadline_ms: 5000 }))
})

compiledTest('compiled launch ignores dotenv, bunfig and inherited runtime switches', async (t) => {
  const fixture = plugin(`export const inject = ['tools']
export function apply(ctx) { ctx.tools.register({ name: 'env', description: '', parameters: { type: 'object', properties: {} }, execute: () => JSON.stringify({ dotenv: process.env.CW_COMPILED_DOTENV, cli: process.env.BUN_BE_BUN, options: process.env.BUN_OPTIONS, execArgv: process.execArgv }) }) }
`)
  writeFileSync(join(fixture.dir, '.env'), 'CW_COMPILED_DOTENV=should-not-load\n')
  writeFileSync(join(fixture.dir, 'bunfig.toml'), 'preload = ["./missing-preload.mjs"]\n')
  const host = await start({ cwd: fixture.dir, env: { BUN_BE_BUN: '1', BUN_OPTIONS: '--preload=missing-preload.mjs' } })
  t.after(async () => { await host.stop(); fixture.cleanup() })
  const { result } = await activate(host, 'env', fixture.entry)
  assert.equal(result.status, 'ok', result.diagnostic)
  const handle = host.registry.find((entry) => entry.op === 'register').handle
  const value = await host.call('tool/call', { handle, call_id: 'env', input: {}, deadline_ms: 5000 })
  const expectedExecArgv = process.platform === 'win32'
    // The Windows LPAC cannot open NUL; compile-host intentionally omits this flag there.
    ? ['--no-install', '--no-env-file', '--no-addons']
    : ['--no-install', '--no-env-file', '--config=/dev/null', '--no-addons']
  assert.deepEqual(JSON.parse(value.content[0].text), { cli: '0', options: '', execArgv: expectedExecArgv })
})

compiledTest('compiled Native imports cannot reach FFI or fresh worker realms', async (t) => {
  const host = await start()
  t.after(() => host.stop())
  for (const [name, source, pattern] of [
    ['ffi', "export async function apply() { const { dlopen } = await import('bun:ffi'); dlopen('/nonexistent/libc.so', {}) }", /bun:ffi.*not available/],
    ['ffi-require', "import { createRequire } from 'node:module'; export function apply() { createRequire(import.meta.url)('bun:ffi').dlopen('/nonexistent/libc.so', {}) }", /bun:ffi.*not available/],
    ['worker', "import { Worker } from 'node:worker_threads'; export function apply() { new Worker('0', { eval: true }) }", /Worker.*not available/],
  ]) {
    const fixture = plugin(source)
    t.after(fixture.cleanup)
    const { result } = await activate(host, name, fixture.entry)
    assert.equal(result.status, 'failed')
    assert.match(result.diagnostic, pattern)
  }
})

compiledTest('compiled host refuses missing npm imports without registry traffic', async (t) => {
  const requests = []
  const server = createServer((request, response) => { requests.push(request.url); response.statusCode = 404; response.end('{}') })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  const url = `http://127.0.0.1:${server.address().port}/`
  const fixture = plugin("import 'codewhale-compiled-missing-package-0101'; export function apply() {}")
  const host = await start({ env: { BUN_CONFIG_REGISTRY: url, NPM_CONFIG_REGISTRY: url, BUN_INSTALL_CACHE_DIR: join(fixture.dir, 'cache') } })
  t.after(async () => { await host.stop(); fixture.cleanup(); await new Promise((resolve) => server.close(resolve)) })
  const { result } = await activate(host, 'missing', fixture.entry)
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /Cannot find package.*codewhale-compiled-missing-package/)
  assert.deepEqual(requests, [])
  const bun = process.env.CODEWHALE_BUN_TEST_BINARY
  assert.ok(bun, 'compiled qualification must name its local Bun control binary')
  const control = spawn(bun, [fixture.entry], { env: { ...process.env, BUN_OPTIONS: '', BUN_BE_BUN: '0', BUN_CONFIG_REGISTRY: url, NPM_CONFIG_REGISTRY: url, BUN_INSTALL_CACHE_DIR: join(fixture.dir, 'cache') }, stdio: 'ignore' })
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => { control.kill(); reject(new Error('local-registry control timed out')) }, 5000)
    control.on('error', (error) => { clearTimeout(timer); reject(error) })
    control.on('exit', () => { clearTimeout(timer); resolve() })
  })
  assert.ok(requests.length > 0, 'without --no-install the same missing import must reach the local control registry')
})

// Five direct-image cases apply on every supported OS. This additional macOS
// reexec probe is registered only where it applies. All platforms separately
// require actual compiled-image memory enforcement in the Rust Native receipt.
if (process.platform === 'darwin') compiledTest('compiled macOS reexec keeps PID and kernel kills memory overflow', async (t) => {
  const fixture = plugin(`export const inject = ['tools']
export function apply(ctx) {
  ctx.tools.register({ name: 'pid', description: '', parameters: { type: 'object', properties: {} }, execute: () => String(process.pid) })
  ctx.tools.register({ name: 'hog', description: '', parameters: { type: 'object', properties: {} }, async execute() { const chunks = []; for (let i = 0; i < 32; i++) { chunks.push(Buffer.alloc(64 * 1024 * 1024, 1)); await new Promise((resolve) => setTimeout(resolve, 5)) }; return String(chunks.length) } })
}`)
  const host = await start({ env: { CODEWHALE_HOST_MEMORY_LIMIT_MIB: '300' } })
  t.after(async () => { await host.stop(); fixture.cleanup() })
  assert.equal(host.hello.memory_limit_mib, 300)
  const { result } = await activate(host, 'memory', fixture.entry)
  assert.equal(result.status, 'ok', result.diagnostic)
  const [pid, hog] = host.registry.filter((entry) => entry.op === 'register').map((entry) => entry.handle)
  const value = await host.call('tool/call', { handle: pid, call_id: 'pid', input: {}, deadline_ms: 5000 })
  assert.equal(Number(value.content[0].text), host.child.pid)
  host.request('tool/call', { handle: hog, call_id: 'hog', input: {}, deadline_ms: 30000 })
  let timer
  const ended = await Promise.race([host.exit, new Promise((resolve) => { timer = setTimeout(() => resolve('still running'), 20000) })])
  clearTimeout(timer)
  assert.deepEqual(ended, { code: null, signal: 'SIGKILL' })
})
