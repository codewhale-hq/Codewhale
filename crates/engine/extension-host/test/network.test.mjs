import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'

const here = dirname(fileURLToPath(import.meta.url))
let credentialsAdapter
let brokeredFetch

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-network-'))
  const outfile = join(dir, 'network.mjs')
  await build({ entryPoints: [join(here, '../src/dsh/network.ts')], outfile, bundle: true, platform: 'node', format: 'esm', packages: 'external', absWorkingDir: join(here, '..'), logLevel: 'silent' })
  ;({ credentialsAdapter, brokeredFetch } = await import(pathToFileURL(outfile).href))
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

function brokerHost(answers) {
  const calls = []
  return {
    calls,
    host: {
      tier: 'plugin',
      broker: async (ctx, service, method, input) => {
        calls.push({ service, method, input })
        const answer = answers[method]
        if (answer instanceof Error) throw answer
        return typeof answer === 'function' ? answer(input) : answer
      },
    },
  }
}

test('credentials resolve to an opaque handle and go only to the network broker', async () => {
  const { host, calls } = brokerHost({
    'credentials.resolve': { ref: 'KEY', value: 'codewhale-credential:KEY' },
    'credentials.describe': { ref: 'KEY', set: true },
    'credentials.set': { ref: 'KEY', set: true },
  })
  const Credentials = credentialsAdapter.define(host)
  const service = Object.create(Credentials.prototype)
  service.ctx = {}
  assert.deepEqual(await service.resolve('KEY'), { value: 'codewhale-credential:KEY' })
  assert.deepEqual(await service.describe('KEY'), { ref: 'KEY', set: true })
  await service.set('KEY', 'sk-secret')
  assert.deepEqual(calls.map((c) => [c.service, c.method]), [
    ['network', 'credentials.resolve'], ['network', 'credentials.describe'], ['network', 'credentials.set'],
  ])
  await assert.rejects(service.resolve(''), /non-empty/)
})

test('a resolve answer that is not a handle is refused rather than passed on', async () => {
  const { host } = brokerHost({ 'credentials.resolve': { value: 'sk-raw' } })
  const service = Object.create(credentialsAdapter.define(host).prototype)
  service.ctx = {}
  await assert.rejects(service.resolve('KEY'), /invalid credential handle/)
  const unset = Object.create(credentialsAdapter.define(brokerHost({ 'credentials.resolve': { value: null } }).host).prototype)
  unset.ctx = {}
  assert.equal(await unset.resolve('KEY'), undefined)
})

test('brokeredFetch sends a typed fetch and rebuilds a Response from the broker answer', async () => {
  const { host, calls } = brokerHost({
    fetch: {
      status: 201, url: 'https://api.example.com/v1/x', redirected: true,
      headers: [['content-type', 'application/json']],
      body_base64: Buffer.from('{"ok":true}').toString('base64'),
    },
  })
  const response = await brokeredFetch(host, {}, 'https://api.example.com/v1/x', {
    method: 'POST', headers: { authorization: 'Bearer codewhale-credential:KEY' }, body: 'hi',
  })
  assert.equal(response.status, 201)
  assert.equal(response.url, 'https://api.example.com/v1/x')
  assert.equal(response.redirected, true)
  assert.deepEqual(await response.json(), { ok: true })
  assert.equal(calls[0].service, 'network')
  assert.equal(calls[0].method, 'fetch')
  assert.deepEqual(calls[0].input.headers, [['authorization', 'Bearer codewhale-credential:KEY']])
  assert.equal(Buffer.from(calls[0].input.body_base64, 'base64').toString(), 'hi')
})

test('a broker refusal reaches the plugin as the refusal', async () => {
  const { host } = brokerHost({ fetch: new Error('egress refused: `evil.example` is not one of this plugin\'s reviewed network hosts') })
  await assert.rejects(brokeredFetch(host, {}, 'https://evil.example/'), /egress refused/)
})
