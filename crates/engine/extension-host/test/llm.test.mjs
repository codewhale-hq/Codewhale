import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'

const here = dirname(fileURLToPath(import.meta.url))
let llmAdapter
let promptOf

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-llm-'))
  const outfile = join(dir, 'llm.mjs')
  await build({ entryPoints: [join(here, '../src/dsh/llm.ts')], outfile, bundle: true, platform: 'node', format: 'esm', packages: 'external', absWorkingDir: join(here, '..'), logLevel: 'silent' })
  ;({ llmAdapter, promptOf } = await import(pathToFileURL(outfile).href))
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

const catalog = {
  providers: [{ id: 'session', name: 'Codewhale session route' }],
  models: [{ provider: 'session', id: 'm-1', name: 'm-1', context_window: 1000, default_max_output_tokens: 4096, max_output_tokens: 16384 }],
  limits: {},
}
const reported = { coverage: 'reported', input_tokens: 5, output_tokens: 3, cache_read_tokens: null, cache_write_tokens: null, reasoning_tokens: null }
const completion = (usage) => ({ text: 'hello', finish: 'stop', route: { provider: 'p', model: 'm-1' }, usage, receipt: { requests: 1, duration_ms: 2 } })

function service(answers) {
  const calls = []
  const host = {
    tier: 'plugin',
    broker: async (ctx, name, method, input) => {
      calls.push({ name, method, input })
      const answer = answers[method]
      if (answer instanceof Error) throw answer
      return answer
    },
  }
  const instance = Object.create(llmAdapter.define(host).prototype)
  instance.ctx = {}
  return { llm: instance, calls }
}

async function drain(stream) {
  const chunks = []
  for await (const chunk of stream) chunks.push(chunk)
  return chunks
}

test('a consumer-shaped stream is one typed complete call and ends with usage and a finish', async () => {
  const { llm, calls } = service({ complete: completion(reported) })
  const chunks = await drain(llm.stream({
    provider: 'session', model: 'm-1', system: 'be brief', maxTokens: 64,
    messages: [{ role: 'user', content: [{ type: 'text', text: 'say ' }, { type: 'text', text: 'hello' }] }],
  }))
  assert.deepEqual(calls, [{ name: 'llm', method: 'complete', input: { prompt: 'say hello', system: 'be brief', provider: 'session', model: 'm-1', max_output_tokens: 64 } }])
  assert.deepEqual(chunks.map((chunk) => chunk.type), ['block-start', 'text-delta', 'block-end', 'usage', 'finish'])
  assert.equal(chunks[1].text, 'hello')
  assert.deepEqual(chunks[3].usage, { inputTokens: 5, outputTokens: 3, totalTokens: 8 })
  assert.deepEqual(chunks[4].reason, { kind: 'stop' })
})

test('unreported usage yields no usage chunk, and a refusal or abort is a terminal finish', async () => {
  const missing = { ...reported, coverage: 'missing', input_tokens: null, output_tokens: null }
  const quiet = await drain(service({ complete: completion(missing) }).llm.stream({ messages: [{ role: 'user', content: 'hi' }] }))
  assert.deepEqual(quiet.map((chunk) => chunk.type), ['block-start', 'text-delta', 'block-end', 'finish'])
  const refused = await drain(service({ complete: new Error('the model call ran out of time') }).llm.stream({ messages: [{ role: 'user', content: 'hi' }] }))
  assert.deepEqual(refused, [{ type: 'finish', reason: { kind: 'error', failure: { message: 'the model call ran out of time', code: 'BROKER_REFUSED' } } }])
  const { llm, calls } = service({ complete: completion(reported) })
  const aborted = await drain(llm.stream({ messages: [{ role: 'user', content: 'hi' }], signal: AbortSignal.abort() }))
  assert.equal(aborted[0].reason.kind, 'aborted')
  assert.deepEqual(calls, [])
  const controller = new AbortController()
  const host = {
    tier: 'plugin',
    broker: async () => {
      controller.abort()
      throw new Error('withdrawn')
    },
  }
  const aborting = Object.create(llmAdapter.define(host).prototype)
  aborting.ctx = {}
  const midFlightAbort = await drain(aborting.stream({ messages: [{ role: 'user', content: 'hi' }], signal: controller.signal }))
  assert.deepEqual(midFlightAbort, [{ type: 'finish', reason: { kind: 'aborted', failure: { message: 'the model call was aborted', code: 'ABORTED' } } }])
})

test('tools, non-text content and tool results are refused locally, and a transcript keeps its roles', async () => {
  const { llm, calls } = service({ complete: completion(reported) })
  await assert.rejects(drain(llm.stream({ messages: [{ role: 'user', content: 'x' }], tools: [{ name: 't' }] })), /without tools/)
  await assert.rejects(drain(llm.stream({ messages: [{ role: 'user', content: 'x' }], tools: { read: {} } })), /without tools/)
  await assert.rejects(drain(llm.stream({ messages: [{ role: 'user', content: [{ type: 'image', attachment: {} }] }] })), /text blocks only/)
  await assert.rejects(drain(llm.stream({ messages: [{ role: 'tool', content: [] }] })), /tool results/)
  await assert.rejects(drain(llm.stream({ messages: [] })), /at least one message/)
  await assert.rejects(llm.complete({}), /prompt/)
  await assert.rejects(llm.complete({ prompt: 'x', maxTokens: Number.NaN }), /maxTokens/)
  await assert.rejects(llm.complete({ prompt: 'x', maxTokens: -1 }), /maxTokens/)
  await assert.rejects(llm.complete({ prompt: 'x', timeoutMs: 0 }), /timeoutMs/)
  await assert.rejects(llm.complete({ prompt: 'x', timeoutMs: Number.POSITIVE_INFINITY }), /timeoutMs/)
  await assert.rejects(llm.complete({ prompt: 'x', provider: 42 }), /provider/)
  assert.deepEqual(calls, [])
  assert.equal(promptOf([{ role: 'user', content: 'a' }, { role: 'assistant', content: [{ type: 'text', text: 'b' }] }]), 'user: a\n\nassistant: b')
})

test('the catalog is read-only and adapter, provider and key registration are named refusals', async () => {
  const { llm, calls } = service({ catalog })
  assert.deepEqual(await llm.listModels('session'), [{ provider: 'session', id: 'm-1', name: 'm-1', inputModalities: ['text'], context: { contextWindow: 1000 }, defaultMaxTokens: 4096 }])
  assert.deepEqual(await llm.listModels('openai'), [])
  assert.equal((await llm.resolveModelInfo('session', 'm-1')).id, 'm-1')
  await assert.rejects(llm.resolveModelInfo('session', 'other'), /only the models in its catalog/)
  assert.equal(llm.usage, undefined)
  assert.ok(calls.every((call) => call.name === 'llm' && call.method === 'catalog'))
  for (const method of ['listProviders', 'registerAdapter', 'registerConfigurableProviders', 'registerModelDiscovery', 'discoverModels', 'prepareCall', 'resolveCallConfig']) {
    assert.throws(() => llm[method](), (error) => error.code === -32001 && error.message.includes(`llm.${method} is not available`))
  }
  await assert.rejects(service({ catalog: { providers: [] } }).llm.catalog(), /invalid llm broker catalog/)
  await assert.rejects(service({ complete: { ...completion(reported), finish: '' } }).llm.complete({ prompt: 'hi' }), /invalid llm broker completion/)
})
