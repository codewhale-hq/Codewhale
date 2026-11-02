import test from 'node:test'
import assert from 'node:assert/strict'
import { createHarnessModule, normalizeOutput } from '../dist/builtin/harness.mjs'
const owner = { plugin_id: 'host:harness', generation: 3, owner_token: 'fixture-token' }
const params = (execution_id = 'one', extra = {}) => ({ owner, execution_id, ticket: 'opaque-exact-fixture-ticket', deadline_ms: 1000, ...extra })
const signal = () => new AbortController().signal
const tick = () => new Promise(resolve => setImmediate(resolve))

test('script result JSON preserves soft failure and metadata', () => {
  assert.deepEqual(normalizeOutput({ success: true, stdout: JSON.stringify({ content: 'soft', success: false, metadata: { structured: [1, 2] } }), stderr: '' }), { ok: true, result: { content: 'soft', success: false, metadata: { structured: [1, 2] } } })
})
test('plain and non-ToolResult JSON retain original stdout', () => {
  for (const stdout of ['hello\n', '{"other":true}', 'null']) assert.deepEqual(normalizeOutput({ success: true, stdout, stderr: '' }), { ok: true, result: { content: stdout, success: true } })
})
test('nonzero process exit keeps both streams in failure', () => { assert.deepEqual(normalizeOutput({ success: false, stdout: 'out', stderr: 'err' }), { ok: false, error: 'out\nerr' }) })
test('real Node Builtin redeems only opaque exact operation fields', async () => {
  const calls = []; const runner = createHarnessModule({ async request(method, args) { calls.push([method, args]); return { success: true, stdout: 'ok', stderr: '' } } }, owner)
  assert.equal((await runner.run(params(), signal())).result.content, 'ok')
  assert.deepEqual(calls, [['exec/redeem', { owner, execution_id: 'one', ticket: 'opaque-exact-fixture-ticket' }]])
  await runner.dispose()
})
test('wrong owner, generation and token never reach Rust', async () => {
  let calls = 0; const runner = createHarnessModule({ async request() { calls++; return {} } }, owner)
  for (const wrong of [{ ...owner, plugin_id: 'native' }, { ...owner, generation: 4 }, { ...owner, owner_token: 'old' }]) await assert.rejects(runner.run(params('one', { owner: wrong }), signal()), /owner is stale/)
  assert.equal(calls, 0); await runner.dispose()
})
test('duplicate pending operation refuses without redeeming twice', async () => {
  let finish; let calls = 0; const runner = createHarnessModule({ request() { calls++; return new Promise(resolve => { finish = resolve }) } }, owner)
  const first = runner.run(params(), signal()); await tick()
  await assert.rejects(runner.run(params(), signal()), /not admitted/)
  finish({ success: true, stdout: '', stderr: '' }); await first; assert.equal(calls, 1); await runner.dispose()
})
test('caller cancel settles even if a fake Rust peer ignores abort', async () => {
  const controller = new AbortController(); let remoteSignal
  const runner = createHarnessModule({ request(_method, _params, supplied) { remoteSignal = supplied; return new Promise(() => {}) } }, owner)
  const pending = runner.run(params(), controller.signal); controller.abort()
  await assert.rejects(pending, /cancelled/); assert.equal(remoteSignal.aborted, true); await runner.dispose()
})
test('owner disposal cancels admitted work and refuses reuse', async () => {
  const runner = createHarnessModule({ request() { return new Promise(() => {}) } }, owner)
  const pending = runner.run(params(), signal()); const rejected = assert.rejects(pending, /cancelled/)
  await runner.dispose(); await rejected; await assert.rejects(runner.run(params('two'), signal()), /stale/)
})
test('deadline reaches broker cancellation without a second execution', async () => {
  let remote; const runner = createHarnessModule({ request(_method, _params, supplied) { remote = supplied; return new Promise(() => {}) } }, owner)
  await assert.rejects(runner.run(params('deadline', { deadline_ms: 5 }), signal()), /cancelled/); assert.equal(remote.aborted, true); await runner.dispose()
})
test('runner refuses malformed and oversized output without truncating', () => {
  for (const output of [{ success: true, stdout: 1, stderr: '' }, { success: true, stdout: 'x'.repeat(1024 * 1024 + 1), stderr: '' }, { success: true, stdout: '', stderr: 'x'.repeat(64 * 1024 + 1) }]) assert.throws(() => normalizeOutput(output), /invalid or oversized/)
})
test('bounded pending operations cancel completely on disposal', async () => {
  let count = 0; const runner = createHarnessModule({ request() { count++; return new Promise(() => {}) } }, owner)
  const pending = Array.from({ length: 32 }, (_, i) => runner.run(params(String(i)), signal()).catch(error => error.message))
  await assert.rejects(runner.run(params('overflow'), signal()), /not admitted/); assert.equal(count, 32)
  await runner.dispose(); assert.equal((await Promise.all(pending)).every(value => value === 'execution cancelled'), true)
})
