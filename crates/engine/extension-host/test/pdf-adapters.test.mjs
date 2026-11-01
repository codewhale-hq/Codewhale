import test from 'node:test'
import assert from 'node:assert/strict'
import { transformStockSnapshot } from '../dist/stock-adapters.mjs'
import { createHarnessModule } from '../dist/builtin/harness.mjs'
const complete = extra => ({ kind: 'pdf_process', state: 'complete', success: true, exit_code: 0, stdout_truncated: false, stderr_truncated: false, stderr: '', ...extra })
const decision = input => transformStockSnapshot(input).result.metadata

test('PDF success is a directive and never carries extracted document bytes', () => {
  const value = transformStockSnapshot(complete({}))
  assert.deepEqual(value.result.metadata, { kind: 'pdf_decision', code: 'success' })
  assert.equal(value.result.content, '')
})
test('PDF stdout overflow wins over process failure and stderr', () => {
  assert.deepEqual(decision(complete({ success: false, exit_code: 2, stdout_truncated: true, stderr: 'bad input' })), { kind: 'pdf_decision', code: 'execution', message: 'pdftotext output exceeded the 16777216 byte safety limit' })
})
test('PDF errors preserve exit-code, empty stderr, signal and truncation spelling', () => {
  assert.equal(decision(complete({ success: false, exit_code: 2, stderr: 'bad\ufffd[31m' })).message, 'pdftotext failed (exit Some(2)): bad\ufffd[31m')
  assert.equal(decision(complete({ success: false, exit_code: null })).message, 'pdftotext failed (exit None): no diagnostic output')
  assert.equal(decision(complete({ success: false, exit_code: -7, stderr: 'diagnostic', stderr_truncated: true })).message, 'pdftotext failed (exit Some(-7)): diagnostic [truncated]')
})
test('PDF typed faults retain their captured status and diagnostic', () => {
  for (const state of ['binary_unavailable', 'cancelled', 'timed_out', 'execution']) assert.deepEqual(decision({ kind: 'pdf_process', state, message: 'exact diagnostic' }), { kind: 'pdf_decision', code: state, message: 'exact diagnostic' })
})
test('PDF malformed and oversized projections refuse', () => {
  for (const value of [complete({ success: 'true' }), complete({ stdout_truncated: null }), complete({ stderr_truncated: 0 }), complete({ exit_code: undefined }), complete({ exit_code: 1.1 }), complete({ exit_code: 2 ** 31 }), complete({ stderr: null }), complete({ stderr: 'x'.repeat(1024 * 1024) }), { kind: 'pdf_process', state: 'retry', message: 'retry' }]) assert.throws(() => transformStockSnapshot(value))
})
test('actual compiled harness redeems the exact opaque PDF job once', async () => {
  const owner = { plugin_id: 'host:harness', generation: 3, owner_token: 'fixture-owner' }
  const calls = []
  const runner = createHarnessModule({ async request(method, params) { calls.push([method, params]); return complete({}) } }, owner)
  const value = await runner.run({ owner, execution_id: 'private-pdf-job', ticket: 'single-use-grant', deadline_ms: 1000 }, new AbortController().signal)
  assert.deepEqual(value.result.metadata, { kind: 'pdf_decision', code: 'success' })
  assert.deepEqual(calls, [['exec/redeem', { owner, execution_id: 'private-pdf-job', ticket: 'single-use-grant' }]])
  assert.equal(JSON.stringify(calls).includes('pdftotext'), false)
  await runner.dispose()
})
test('PDF runner cancellation refuses late output and never replays the process', async () => {
  const owner = { plugin_id: 'host:harness', generation: 3, owner_token: 'fixture-owner' }
  const abort = new AbortController()
  let finish, calls = 0
  const runner = createHarnessModule({ request() { calls++; return new Promise(resolve => { finish = resolve }) } }, owner)
  const pending = runner.run({ owner, execution_id: 'private-pdf-job', ticket: 'single-use-grant', deadline_ms: 1000 }, abort.signal)
  abort.abort()
  await assert.rejects(pending, /cancelled/)
  finish(complete({})); assert.equal(calls, 1)
  await runner.dispose()
})
