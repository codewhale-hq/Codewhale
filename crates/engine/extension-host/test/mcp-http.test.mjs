// Real official SDK HTTP/SSE transports in the committed builtin bundle.
// FetchProxy network/ticket authority is simulated here; Rust acceptance tests
// separately run the real broker and guarded McpHttpClient against loopback.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { createMcpModule } from '../dist/builtin/mcp.mjs'
import { validateMessage } from '../dist/protocol.mjs'

const owner = { plugin_id: 'host:mcp', generation: 1, owner_token: 'builtin-test-token' }
const initParams = { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'codewhale-tui', version: '0.10.1' } }
const metadata = { 'io.modelcontextprotocol/protocolVersion': '2026-07-28', 'io.modelcontextprotocol/clientInfo': initParams.clientInfo, 'io.modelcontextprotocol/clientCapabilities': {} }
class FakeFetchProxy {
  session = randomUUID()
  grants = new Map()
  bodies = new Map()
  frames = []
  fetches = []
  closed = false
  modern = false
  sequence = 0
  failWrite = false
  status = undefined
  silent = false
  constructor(legacy = false, ssePost = false) { this.legacy = legacy; this.ssePost = ssePost }
  grant(method, params) {
    if (this.modern && !['initialize', 'notifications/initialized'].includes(method)) params = { ...params, _meta: metadata }
    const grant = { ticket: randomUUID(), operation_id: randomUUID(), method, ...(method.startsWith('notifications/') ? {} : { wire_id: String(this.sequence++) }), params: structuredClone(params) }
    this.grants.set(grant.ticket, structuredClone(grant)); return grant
  }
  body(bytes, open = false) { const id = randomUUID(); this.bodies.set(id, { bytes: Array.from(Buffer.from(bytes)), open, waiting: undefined }); return id }
  append(id, bytes) { const body = this.bodies.get(id); if (!body) return; body.bytes.push(...Buffer.from(bytes)); if (body.waiting) { const take = body.waiting; body.waiting = undefined; take() } }
  async request(method, params, signal) {
    assert.deepEqual(params.owner, owner); assert.equal(params.session_id, this.session)
    if (method === 'net/start') { assert.equal(params.ticket, 'launch-once'); return {} }
    if (method === 'net/close') { this.closed = true; for (const body of this.bodies.values()) body.waiting?.(); this.bodies.clear(); return {} }
    if (method === 'net/release') { this.bodies.delete(params.response_id); return {} }
    if (method === 'net/read') {
      const body = this.bodies.get(params.response_id); assert.ok(body)
      if (!body.bytes.length && body.open) await new Promise((resolve, reject) => {
        const abort = () => { body.waiting = undefined; reject(new Error('fake response cancelled')) }
        body.waiting = () => { signal?.removeEventListener('abort', abort); resolve() }
        if (signal?.aborted) abort(); else signal?.addEventListener('abort', abort, { once: true })
      })
      if (this.closed) throw new Error('fake response closed')
      const data = body.bytes.splice(0, 32768); return { data, done: !body.open && !body.bytes.length }
    }
    assert.equal(method, 'net/fetch')
    this.fetches.push(structuredClone(params))
    assert.equal(new URL(params.url).origin, 'https://mcp-proxy.invalid')
    assert.ok(Object.keys(params.headers).every(key => ['accept', 'content_type', 'mcp_session_id', 'mcp_protocol_version', 'mcp_method', 'mcp_name'].includes(key)))
    if (params.method === 'GET') {
      assert.equal(params.url, `https://mcp-proxy.invalid/${this.session}`)
      if (!this.legacy) return { status: 405, headers: {} }
      this.stream = this.body(this.holdEndpoint ? '' : `event: endpoint\ndata: https://mcp-proxy.invalid/${this.session}/endpoint\n\n`, true)
      return { status: 200, headers: { 'content-type': 'text/event-stream' }, response_id: this.stream }
    }
    assert.equal(params.method, 'POST')
    assert.equal(params.url, `https://mcp-proxy.invalid/${this.session}${this.legacy ? '/endpoint' : ''}`)
    const frame = params.frame, grant = this.grants.get(params.ticket)
    assert.ok(grant, 'operation grant is single-use')
    assert.equal(params.operation_id, grant.operation_id); assert.equal(frame.method, grant.method)
    assert.deepEqual(frame.params ?? {}, grant.params)
    if (grant.wire_id !== undefined) assert.equal(frame.id, grant.wire_id)
    this.grants.delete(params.ticket); this.frames.push(structuredClone(frame))
    if (this.negotiate && !this.legacy) {
      this.legacy = true
      const replacement = { ...structuredClone(grant), ticket: randomUUID() }
      if (this.mutateNegotiation) replacement.params = { forged: true }
      this.grants.set(replacement.ticket, replacement)
      return { status: 405, headers: {}, legacy_grant: replacement }
    }
    if (this.failWrite && frame.method === 'tools/call') throw new Error('fake unknown partial HTTP write')
    if (this.status !== undefined && frame.method !== 'initialize') return { status: this.status, headers: {} }
    if (!Object.hasOwn(frame, 'id')) return { status: 202, headers: {} }
    assert.equal(typeof frame.id, 'string', 'real numeric SDK IDs must not reach the peer')
    const result = this.customResult?.(frame.method) ?? (frame.method === 'server/discover' ? { resultType: 'complete', ttlMs: 0, cacheScope: 'private', supportedVersions: ['2026-07-28'], capabilities: { tools: {} } } : frame.method === 'initialize' ? { protocolVersion: '2025-06-18', capabilities: { tools: {}, resources: {}, prompts: {} }, serverInfo: { name: 'fetch-fixture', version: '1' } }
      : frame.method === 'tools/list' ? { ...(this.modern ? { ttlMs: 0, cacheScope: 'private' } : {}), tools: [{ name: 'echo', inputSchema: { type: 'object' } }], nextCursor: 'next-page' }
      : { content: [{ type: 'text', text: 'ok' }] })
    const response = JSON.stringify({ jsonrpc: '2.0', id: frame.id, result: this.modern ? { resultType: 'complete', ...result } : result })
    if (this.legacy) {
      if (!this.silent) this.append(this.stream, `event: message\ndata: ${response}\n\n`)
      return { status: 202, headers: {} }
    }
    const headers = { 'content-type': this.ssePost ? 'text/event-stream' : 'application/json', ...(frame.method === 'initialize' ? { 'mcp-session-id': 'rust-observed-session' } : {}) }
    return { status: 200, headers, response_id: this.body(this.silent ? '' : this.ssePost ? `event: message\ndata: ${response}\n\n` : response, this.silent) }
  }
}
async function fixture(legacy = false, ssePost = false, modern = false) {
  const broker = new FakeFetchProxy(legacy, ssePost), module = createMcpModule(broker, owner)
  broker.modern = modern
  const open = { owner, session_id: broker.session, transport: legacy ? 'sse' : 'http', launch_ticket: 'launch-once', initialize_grant: broker.grant('initialize', initParams), initialized_grant: broker.grant('notifications/initialized', {}), client_version: '0.10.1', deadline_ms: 1000 }
  if (modern) open.discover_grant = broker.grant('server/discover', {})
  const signal = new AbortController().signal
  await module.open(open, signal); return { broker, module, open, signal }
}
for (const [name, legacy, ssePost, modern] of [['modern HTTP JSON', false, false, true], ['HTTP SSE response', false, true, false], ['legacy SSE', true, false, false]]) {
  test(`official ${name} transport uses opaque FetchProxy and one exact per-page Rust wire ID`, async () => {
    const f = await fixture(legacy, ssePost, modern)
    try {
      const grant = f.broker.grant('tools/list', { cursor: 'one-page' })
      const result = await f.module.request({ owner, session_id: f.open.session_id, grant, deadline_ms: 1000 }, f.signal)
      assert.equal(result.nextCursor, 'next-page')
      assert.equal(f.broker.frames.filter(frame => frame.method === 'tools/list').length, 1)
      assert.equal(f.broker.frames.find(frame => frame.method === 'tools/list').id, grant.wire_id)
      assert.ok(f.broker.frames.filter(frame => Object.hasOwn(frame, 'id')).every(frame => typeof frame.id === 'string'))
      if (!legacy && !modern) assert.equal(f.broker.fetches.find(params => params.frame?.method === 'tools/list').headers.mcp_session_id, 'rust-observed-session')
      if (modern) {
        assert.equal(f.broker.frames.some(frame => frame.method === 'initialize'), false)
        await f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/call', { name: 'echo', arguments: {} }), deadline_ms: 1000 }, f.signal)
        for (const request of f.broker.fetches.filter(p => p.frame)) {
          assert.deepEqual(request.frame.params._meta, metadata)
          assert.equal(request.headers.mcp_protocol_version, '2026-07-28')
          assert.equal(request.headers.mcp_method, request.frame.method)
          assert.equal(request.headers.mcp_name, request.frame.method === 'tools/call' ? 'echo' : undefined)
          assert.equal(request.headers.mcp_session_id, undefined)
        }
      }
    } finally { await f.module.dispose() }
    assert.equal(f.broker.closed, true)
  })
}

test('HTTP decoded grant mismatch fails before a FetchProxy write and closes the session', async () => {
  const f = await fixture()
  const grant = f.broker.grant('tools/call', { name: 'echo', arguments: {} })
  grant.params.arguments = { forged: true }
  await assert.rejects(f.module.request({ owner, session_id: f.open.session_id, grant, deadline_ms: 1000 }, f.signal), /exact|Connection closed/)
  assert.equal(f.broker.frames.some(frame => frame.method === 'tools/call'), false)
  assert.equal(f.broker.closed, true); await f.module.dispose()
})
test('unknown partial HTTP write retires once and never replays', async () => {
  const f = await fixture(); f.broker.failWrite = true
  await assert.rejects(f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/call', { name: 'echo', arguments: {} }), deadline_ms: 1000 }, f.signal), /partial HTTP write|Connection closed/)
  assert.equal(f.broker.frames.filter(frame => frame.method === 'tools/call').length, 1)
  assert.equal(f.broker.closed, true); await f.module.dispose()
})
test('401 cannot make SDK fetch replay a consumed operation', async () => {
  const f = await fixture(); f.broker.status = 401
  await assert.rejects(f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/list', {}), deadline_ms: 1000 }, f.signal))
  assert.equal(f.broker.frames.filter(frame => frame.method === 'tools/list').length, 1)
  assert.equal(f.broker.closed, true); await f.module.dispose()
})
test('plugin tier cannot redeem any FetchProxy frame', () => {
  const rows = [['net/start', { ticket: 't' }], ['net/fetch', { url: 'https://mcp-proxy.invalid/s', method: 'GET', headers: {} }], ['net/read', { response_id: 'r' }], ['net/release', { response_id: 'r' }], ['net/close', {}]]
  for (const [method, rest] of rows) {
    const frame = { jsonrpc: '2.0', id: 1, method, params: { owner, session_id: 's', ...rest } }
    assert.throws(() => validateMessage(frame, 'host_to_core', 'plugin'), /not allowed/)
    validateMessage(frame, 'host_to_core', 'builtin')
  }
})
test('cancelled HTTP body releases its response and refuses queued work', async () => {
  const f = await fixture(); f.broker.silent = true
  const first = f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/list', {}), deadline_ms: 20 }, f.signal)
  const queued = f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('prompts/list', {}), deadline_ms: 1000 }, f.signal)
  const firstResult = assert.rejects(first, /timed out|closed|cancelled/)
  const queuedResult = assert.rejects(queued, /closed|cancelled|stale/)
  await firstResult; await queuedResult
  assert.equal(f.broker.frames.filter(frame => frame.method === 'tools/list').length, 1)
  assert.equal(f.broker.frames.some(frame => frame.method === 'prompts/list'), false)
  assert.equal(f.broker.closed, true); assert.equal(f.broker.bodies.size, 0)
  await f.module.dispose()
})

// Actual official Streamable HTTP -> SSE transport change; the FetchProxy
// simulates one explicit Rust refusal and a fresh exact single-use grant.
async function negotiatedFixture(options = {}) {
  const broker = new FakeFetchProxy(), module = createMcpModule(broker, owner)
  Object.assign(broker, { negotiate: true }, options)
  const open = { owner, session_id: broker.session, transport: 'http', launch_ticket: 'launch-once', initialize_grant: broker.grant('initialize', initParams), initialized_grant: broker.grant('notifications/initialized', {}), client_version: '0.10.1', deadline_ms: 100 }
  return { broker, module, open }
}
test('official SDK negotiates legacy SSE only with a fresh exact Rust grant and original wire ID', async () => {
  const f = await negotiatedFixture()
  try {
    await f.module.open(f.open, new AbortController().signal)
    const attempts = f.broker.fetches.filter(p => p.frame?.method === 'initialize')
    assert.equal(attempts.length, 2)
    assert.equal(attempts[0].frame.id, f.open.initialize_grant.wire_id)
    assert.deepEqual(attempts[0].frame, attempts[1].frame)
    assert.notEqual(attempts[0].ticket, attempts[1].ticket)
    assert.equal(attempts[0].operation_id, attempts[1].operation_id)
    assert.equal(new URL(attempts[1].url).pathname.endsWith('/endpoint'), true)
    assert.equal(f.broker.frames.filter(p => p.method === 'notifications/initialized').length, 1)
  } finally { await f.module.dispose() }
})
test('changed negotiation params refuse before SDK opens legacy channel or rewrites a request', async () => {
  const f = await negotiatedFixture({ mutateNegotiation: true })
  await assert.rejects(f.module.open(f.open, new AbortController().signal), /mismatch|closed/)
  assert.equal(f.broker.fetches.some(p => p.method === 'GET'), false)
  assert.equal(f.broker.frames.filter(p => p.method === 'initialize').length, 1)
  assert.equal(f.broker.closed, true); await f.module.dispose()
})
test('cancellation during negotiated endpoint discovery releases body without replaying the admitted request', async () => {
  const f = await negotiatedFixture({ holdEndpoint: true })
  await assert.rejects(f.module.open(f.open, new AbortController().signal), /closed|cancelled/)
  assert.equal(f.broker.frames.filter(p => p.method === 'initialize').length, 1)
  assert.equal(f.broker.closed, true); assert.equal(f.broker.bodies.size, 0)
  await f.module.dispose()
})

test('correlated malformed catalog reply preserves SDK session for Rust per-item admission and the next page', async () => {
  const f = await fixture()
  f.broker.customResult = method => method === 'tools/list' ? { tools: [{ description: 'missing name', inputSchema: { type: 'object' } }, { name: 'valid', inputSchema: { type: 'object' } }], nextCursor: 'next' } : undefined
  await assert.rejects(f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/list', {}), deadline_ms: 1000 }, f.signal))
  assert.equal(f.broker.closed, false, 'the exact correlated reply is available to Rust; schema refusal is not an unknown write')
  f.broker.customResult = undefined
  const result = await f.module.request({ owner, session_id: f.open.session_id, grant: f.broker.grant('tools/list', { cursor: 'next' }), deadline_ms: 1000 }, f.signal)
  assert.equal(result.nextCursor, 'next-page')
  assert.equal(f.broker.frames.filter(p => p.method === 'tools/list').length, 2)
  await f.module.dispose()
})
