// The RPC peer's host-to-core requests: cancellation by AbortSignal.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ErrorCode } from '../dist/protocol.mjs'
import { RpcError, RpcPeer } from '../dist/rpc.mjs'

function peer() {
  const sent = []
  const rpc = new RpcPeer((message) => sent.push(message), 'plugin')
  return { rpc, sent }
}

test('an aborted request sends $/cancel for its id and rejects as cancelled at once', async () => {
  const { rpc, sent } = peer()
  const controller = new AbortController()
  const request = rpc.request('registry/unregister', { owner: { plugin_id: 'p', generation: 1, owner_token: 't' }, handle: 1 }, controller.signal)
  const id = sent[0].id
  assert.equal(sent.length, 1)
  controller.abort()
  await assert.rejects(request, (error) => error instanceof RpcError && error.code === ErrorCode.Cancelled)
  assert.deepEqual(sent[1], { jsonrpc: '2.0', method: '$/cancel', params: { id } })
  // The answer that arrives anyway is dropped: nothing waits for it and nothing throws.
  rpc.handle({ jsonrpc: '2.0', id, result: {} })
  // A second abort is a no-op.
  controller.abort()
  assert.equal(sent.length, 2)
})

test('an already-aborted signal sends nothing, and an answered request is never cancelled afterwards', async () => {
  const { rpc, sent } = peer()
  const aborted = AbortSignal.abort()
  await assert.rejects(rpc.request('registry/unregister', { owner: { plugin_id: 'p', generation: 1, owner_token: 't' }, handle: 1 }, aborted), (error) => error.code === ErrorCode.Cancelled)
  assert.equal(sent.length, 0)

  const controller = new AbortController()
  const request = rpc.request('registry/unregister', { owner: { plugin_id: 'p', generation: 1, owner_token: 't' }, handle: 2 }, controller.signal)
  rpc.handle({ jsonrpc: '2.0', id: sent[0].id, result: {} })
  assert.deepEqual(await request, {})
  controller.abort()
  assert.equal(sent.length, 1, 'no $/cancel for a request that was already answered')
})
