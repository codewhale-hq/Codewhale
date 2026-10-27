// Real official SDK + committed builtin bundle; the Rust broker is a fake.
// Rust launch/ticket/HumanDecision receipts live in extension_host::mcp::tests.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { createMcpModule } from '../dist/builtin/mcp.mjs'
import { validateMessage } from '../dist/protocol.mjs'

const owner = { plugin_id: 'host:mcp', generation: 1, owner_token: 'builtin-test-token' }
const initParams = { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'codewhale-tui', version: '0.10.1' } }
class FakeBroker {
  frames = []
  writes = []
  accepted = []
  grants = new Map()
  nextFacadeId = 1
  serverPings = new Set()
  waiting = undefined
  closed = false
  modern = false
  inputRequired = false
  failWrite = undefined
  silent = undefined
  grant(method, params) {
    const grant = { ticket: randomUUID(), operation_id: randomUUID(), method, ...(method.startsWith('notifications/') ? {} : { wire_id: String(this.nextFacadeId++) }), params: structuredClone(params) }
    this.grants.set(grant.ticket, structuredClone(grant))
    return grant
  }
  push(frame) { if (frame.method === 'ping') this.serverPings.add(JSON.stringify(frame.id)); if (this.waiting) { const take = this.waiting; this.waiting = undefined; take.resolve({ frame }) } else this.frames.push(frame) }
  async request(method, params, signal) {
    assert.deepEqual(params.owner, owner)
    if (method === 'proc/launch') { assert.equal(params.ticket, 'launch-once'); return {} }
    if (method === 'proc/close') { this.closed = true; this.waiting?.reject(new Error('fake broker closed')); this.waiting = undefined; return {} }
    if (method === 'proc/read') {
      if (this.closed) return { closed: true }
      if (this.frames.length) return { frame: this.frames.shift() }
      return new Promise((resolve, reject) => {
        const abort = () => { this.waiting = undefined; reject(new Error('fake read aborted')) }
        this.waiting = { resolve: value => { signal?.removeEventListener('abort', abort); resolve(value) }, reject: error => { signal?.removeEventListener('abort', abort); reject(error) } }
        if (signal?.aborted) abort(); else signal?.addEventListener('abort', abort, { once: true })
      })
    }
    assert.equal(method, 'proc/write')
    const frame = params.frame
    this.writes.push(structuredClone(frame))
    if (frame.method === undefined) {
      assert.ok(this.serverPings.delete(JSON.stringify(frame.id)), 'response must target an observed ping')
      assert.deepEqual(frame.result, {})
    } else if (frame.method !== 'notifications/cancelled') {
      const grant = this.grants.get(params.ticket)
      assert.ok(grant, 'single-use grant must exist')
      assert.equal(grant.operation_id, params.operation_id)
      assert.equal(grant.method, frame.method)
      if (grant.wire_id !== undefined) assert.equal(frame.id, grant.wire_id)
      assert.deepEqual(grant.params, frame.params ?? {})
      this.grants.delete(params.ticket)
    }
    if (this.failWrite !== undefined && frame.method === this.failWrite) throw new Error('fake partial write failure')
    this.accepted.push(structuredClone(frame))
    if (frame.method === undefined || !Object.hasOwn(frame, 'id') || frame.method === this.silent) return {}
    const result = this.inputRequired && frame.method === 'tools/call'
      ? { resultType: 'input_required', requestState: 'server-state' }
      : frame.method === 'server/discover'
      ? { resultType: 'complete', ttlMs: 0, cacheScope: 'private', supportedVersions: ['2026-07-28'], capabilities: { tools: {}, resources: {}, prompts: {} } }
      : frame.method === 'initialize'
      ? { protocolVersion: '2025-06-18', capabilities: { tools: {}, resources: {}, prompts: {} }, serverInfo: { name: 'fixture', version: '1.0' } }
      : frame.method === 'tools/list'
        ? { ...(this.modern ? { ttlMs: 0, cacheScope: 'private' } : {}), tools: [{ name: 'echo', inputSchema: { type: 'object' } }], nextCursor: 'next-page' }
        : frame.method === 'resources/list' ? { resources: [] }
        : frame.method === 'resources/templates/list' ? { resourceTemplates: [] }
        : frame.method === 'prompts/list' ? { prompts: [] }
        : { content: [{ type: 'text', text: 'ok' }], isError: false }
    this.push({ jsonrpc: '2.0', id: frame.id, result: this.modern ? { resultType: 'complete', ...result } : result })
    return {}
  }
}
async function fixture() {
  const broker = new FakeBroker(), module = createMcpModule(broker, owner)
  const open = { owner, session_id: randomUUID(), launch_ticket: 'launch-once', initialize_grant: broker.grant('initialize', initParams), initialized_grant: broker.grant('notifications/initialized', {}), client_version: '0.10.1', deadline_ms: 1000 }
  return { broker, module, open, signal: new AbortController().signal }
}

test('pinned SDK performs modern discovery and preserves per-page cursor without list cache aggregation', async () => {
  const f = await fixture()
  try {
    f.broker.modern = true
    const metadata = { 'io.modelcontextprotocol/protocolVersion': '2026-07-28', 'io.modelcontextprotocol/clientInfo': initParams.clientInfo, 'io.modelcontextprotocol/clientCapabilities': {} }
    f.open.discover_grant = f.broker.grant('server/discover', { _meta: metadata })
    const ready = await f.module.open(f.open, f.signal)
    assert.equal(ready.protocolVersion, '2026-07-28')
    assert.equal(ready.serverInfo, undefined)
    assert.deepEqual(ready.capabilities, { tools: {}, resources: {}, prompts: {} })
    assert.deepEqual(f.broker.accepted.map(f => f.method), ['server/discover'])
    assert.equal(f.broker.accepted[0].id, f.open.discover_grant.wire_id)
    const result = await f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/list', { _meta: metadata }), deadline_ms: 1000 }, f.signal)
    assert.equal(result.nextCursor, 'next-page')
    assert.equal(f.broker.accepted.filter(f => f.method === 'tools/list').length, 1)
    assert.equal(f.broker.accepted.some(f => f.method === 'initialize'), false)
    f.broker.inputRequired = true
    const pending = await f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/call', { name: 'echo', arguments: {}, _meta: metadata }), deadline_ms: 1000 }, f.signal)
    assert.equal(pending.resultType, 'input_required')
    assert.equal(pending.requestState, 'server-state')
    assert.equal(f.broker.accepted.filter(f => f.method === 'tools/call').length, 1)
  } finally { await f.module.dispose() }
  assert.equal(f.broker.closed, true)
})

test('decoded initialize mismatch fails before a broker write', async () => {
  const f = await fixture()
  f.open.initialize_grant.params.capabilities = { sampling: {} }
  await assert.rejects(f.module.open(f.open, f.signal), /exact operation grant/)
  assert.equal(f.broker.writes.length, 0)
  assert.equal(f.broker.closed, true)
})

test('plugin-tier frames cannot redeem imported builtin code, and stale owner identity is refused', async () => {
  const frame = { jsonrpc: '2.0', id: 1, method: 'proc/write', params: { owner, session_id: 's', frame: { jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }, ticket: 't', operation_id: 'o' } }
  assert.throws(() => validateMessage(frame, 'host_to_core', 'plugin'), /not allowed/)
  validateMessage(frame, 'host_to_core', 'builtin')
  assert.throws(() => createMcpModule(new FakeBroker(), { ...owner, plugin_id: 'user/p/foreign' }), /pinned builtin/)
  const f = await fixture()
  await assert.rejects(f.module.open({ ...f.open, owner: { ...owner, generation: 2 } }, f.signal), /no longer live/)
  assert.equal(f.broker.writes.length, 0)
  await f.module.dispose()
})

test('partial write retires the pipe and never replays tools/call', async () => {
  const f = await fixture()
  await f.module.open(f.open, f.signal)
  f.broker.failWrite = 'tools/call'
  await assert.rejects(f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/call', { name: 'echo', arguments: {} }), deadline_ms: 1000 }, f.signal), /partial write|Connection closed/)
  assert.equal(f.broker.closed, true)
  assert.equal(f.broker.writes.filter(f => f.method === 'tools/call').length, 1)
  await f.module.dispose()
})

test('single-use request replay is refused and shuts down the same session', async () => {
  const f = await fixture()
  try {
    await f.module.open(f.open, f.signal)
    const grant = f.broker.grant('tools/list', {})
    await f.module.request({ owner, session_id: f.open.session_id, grant, deadline_ms: 1000 }, f.signal)
    await assert.rejects(f.module.request({ owner, session_id: f.open.session_id, grant, deadline_ms: 1000 }, f.signal), /single-use grant|Connection closed/)
    assert.equal(f.broker.accepted.filter(f => f.method === 'tools/list').length, 1)
    assert.equal(f.broker.closed, true)
  } finally { await f.module.dispose() }
})

test('deadline cancellation targets the admitted request and owner disposal refuses queued work', async () => {
  const f = await fixture()
  await f.module.open(f.open, f.signal)
  f.broker.silent = 'tools/list'
  const first = f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/list', {}), deadline_ms: 20 }, f.signal)
  const queued = f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('prompts/list', {}), deadline_ms: 1000 }, f.signal)
  const waitFirst = assert.rejects(first, /timed out/)
  const waitQueued = assert.rejects(queued, /no longer live|closed|cancelled|stale/)
  await waitFirst
  await f.module.dispose()
  await waitQueued
  const request = f.broker.accepted.find(f => f.method === 'tools/list')
  const cancel = f.broker.writes.find(f => f.method === 'notifications/cancelled')
  assert.equal(cancel?.params.requestId, request.id)
  assert.equal(f.broker.accepted.some(f => f.method === 'prompts/list'), false)
})

test('SDK answers an observed ping with exactly empty result and no operation ticket', async () => {
  const f = await fixture()
  try {
    await f.module.open(f.open, f.signal)
    f.broker.push({ jsonrpc: '2.0', id: 'server-ping', method: 'ping', params: {} })
    await new Promise((resolve, reject) => { const timer = setInterval(() => {
      if (f.broker.accepted.some(frame => frame.id === 'server-ping')) { clearInterval(timer); resolve() }
    }, 1); setTimeout(() => { clearInterval(timer); reject(new Error('ping not answered')) }, 500).unref() })
    const answer = f.broker.accepted.find(frame => frame.id === 'server-ping')
    assert.deepEqual(answer, { jsonrpc: '2.0', id: 'server-ping', result: {} })
  } finally { await f.module.dispose() }
})
