import test, { before, after } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'
import { Context } from '@deepseek-ai/cordis'

const here = dirname(fileURLToPath(import.meta.url))
const OWNER = Symbol.for('codewhale.extension-host.owner')

let tmpDir
let DSH_WEBSERVER_ADAPTERS
let MAX_REQUEST_BODY_BYTES
let MAX_RESPONSE_BODY_BYTES
let clearOwnerRoutes
let dispatchPluginRoute
let registeredRouteCount

before(async () => {
  tmpDir = mkdtempSync(join(here, '..', 'node_modules', '.cw-webserver-'))
  const outfile = join(tmpDir, 'webserver.mjs')
  await build({
    entryPoints: [join(here, '../src/dsh/webserver.ts')],
    outfile,
    bundle: true,
    platform: 'node',
    format: 'esm',
    packages: 'external',
    absWorkingDir: join(here, '..'),
    logLevel: 'silent',
  })
  const mod = await import(pathToFileURL(outfile).href)
  ;({
    DSH_WEBSERVER_ADAPTERS,
    MAX_REQUEST_BODY_BYTES,
    MAX_RESPONSE_BODY_BYTES,
    clearOwnerRoutes,
    dispatchPluginRoute,
    registeredRouteCount,
  } = mod)
})

after(() => {
  if (tmpDir) rmSync(tmpDir, { recursive: true, force: true })
})

async function createHarness() {
  const root = new Context()
  const owners = new Map()
  const adapterHost = {
    tier: 'plugin',
    rpc: {},
    ownerOf: (ctx) => ctx[OWNER],
    liveInvocation: () => undefined,
    brokerTicket: () => { throw new Error('unused') },
    broker: async () => { throw new Error('unused') },
    log: () => {},
  }
  for (const adapter of DSH_WEBSERVER_ADAPTERS) {
    root.plugin(adapter.define(adapterHost))
  }
  await new Promise((resolve) => setTimeout(resolve, 0))

  function makeOwner(pluginId, token, dataDir) {
    const owner = {
      ref: { plugin_id: pluginId, owner_token: token, generation: 1 },
      pluginName: pluginId,
      fibers: [],
      pendingRegistrations: new Set(),
      refusals: [],
      tools: new Map(),
      commands: new Map(),
      shellHooks: new Map(),
      hooks: new Map(),
      promptSections: new Map(),
      skillRoots: new Map(),
      avatarPacks: new Map(),
      mcpDefinitions: new Map(),
      entries: new Set(),
      dataDir,
      state: 'active',
    }
    owners.set(token, owner)
    const ctx = root.extend({ [OWNER]: owner })
    return { owner, ctx }
  }

  return { root, owners, makeOwner }
}

test('webServer exact and prefix routes handle GET/POST JSON round-trip and query strings', async () => {
  const { owners, makeOwner } = await createHarness()
  const { owner, ctx } = makeOwner('plugin-a', 'tok-a', '/tmp/plugin-a')
  try {
    const disposeGet = ctx.webServer.register({
      kind: 'exact',
      path: '/api/ping',
      handler: (req, res) => {
        const url = new URL(req.url, 'http://127.0.0.1')
        res.writeHead(200, { 'content-type': 'application/json', 'x-echo': req.headers['x-custom'] ?? '' })
        res.end(JSON.stringify({ ok: true, q: url.searchParams.get('name'), remote: req.socket.remoteAddress }))
      },
    })
    const disposePost = ctx.webServer.register({
      kind: 'prefix',
      path: '/api/items',
      handler: async (req, res) => {
        const chunks = []
        for await (const chunk of req) chunks.push(chunk)
        const parsed = JSON.parse(Buffer.concat(chunks).toString('utf8'))
        res.setHeader('content-type', 'application/json')
        res.statusCode = 201
        res.end(JSON.stringify({ path: req.url, received: parsed }))
      },
    })

    const getRes = await dispatchPluginRoute(
      { owners },
      {
        owner: owner.ref,
        method: 'GET',
        path: '/api/ping',
        query: 'name=whale',
        headers: [{ name: 'x-custom', value: 'hello' }],
        deadline_ms: 5000,
      },
      new AbortController().signal,
    )
    assert.equal(getRes.matched, true)
    assert.equal(getRes.status, 200)
    assert.deepEqual(JSON.parse(Buffer.from(getRes.body_base64, 'base64').toString('utf8')), {
      ok: true,
      q: 'whale',
      remote: '127.0.0.1',
    })
    assert.ok(getRes.headers.some((h) => h.name === 'x-echo' && h.value === 'hello'))

    const payload = Buffer.from(JSON.stringify({ title: 'Ship 0.10.5' }), 'utf8').toString('base64')
    const postRes = await dispatchPluginRoute(
      { owners },
      {
        owner: owner.ref,
        method: 'POST',
        path: '/api/items/42',
        headers: [{ name: 'content-type', value: 'application/json' }],
        body_base64: payload,
        deadline_ms: 5000,
      },
      new AbortController().signal,
    )
    assert.equal(postRes.matched, true)
    assert.equal(postRes.status, 201)
    assert.deepEqual(JSON.parse(Buffer.from(postRes.body_base64, 'base64').toString('utf8')), {
      path: '/api/items/42',
      received: { title: 'Ship 0.10.5' },
    })

    const miss = await dispatchPluginRoute(
      { owners },
      { owner: owner.ref, method: 'GET', path: '/api/unknown', headers: [], deadline_ms: 5000 },
      new AbortController().signal,
    )
    assert.equal(miss.matched, false)
    assert.equal(miss.status, 404)

    disposeGet()
    disposePost()
    assert.equal(registeredRouteCount('tok-a'), 0)
  } finally {
    clearOwnerRoutes('tok-a')
  }
})

test('webServer enforces strict owner isolation between plugins', async () => {
  const { owners, makeOwner } = await createHarness()
  const a = makeOwner('plugin-a', 'tok-iso-a', '/tmp/a')
  const b = makeOwner('plugin-b', 'tok-iso-b', '/tmp/b')
  try {
    a.ctx.webServer.get('/secret', (_req, res) => {
      res.end('from-a')
    })

    const fromB = await dispatchPluginRoute(
      { owners },
      { owner: b.owner.ref, method: 'GET', path: '/secret', headers: [], deadline_ms: 5000 },
      new AbortController().signal,
    )
    assert.equal(fromB.matched, false)

    const fromA = await dispatchPluginRoute(
      { owners },
      { owner: a.owner.ref, method: 'GET', path: '/secret', headers: [], deadline_ms: 5000 },
      new AbortController().signal,
    )
    assert.equal(fromA.matched, true)
    assert.equal(Buffer.from(fromA.body_base64, 'base64').toString('utf8'), 'from-a')
  } finally {
    clearOwnerRoutes('tok-iso-a')
    clearOwnerRoutes('tok-iso-b')
  }
})

test('webServer enforces request and response body size caps', async () => {
  const { owners, makeOwner } = await createHarness()
  const { owner, ctx } = makeOwner('plugin-cap', 'tok-cap', '/tmp/cap')
  try {
    ctx.webServer.post('/echo', (_req, res) => {
      res.end('ok')
    })
    ctx.webServer.get('/huge', (_req, res) => {
      res.write(Buffer.alloc(MAX_RESPONSE_BODY_BYTES + 1))
      res.end()
    })

    const oversizedReq = Buffer.alloc(MAX_REQUEST_BODY_BYTES + 1).toString('base64')
    await assert.rejects(
      () => dispatchPluginRoute(
        { owners },
        { owner: owner.ref, method: 'POST', path: '/echo', headers: [], body_base64: oversizedReq, deadline_ms: 5000 },
        new AbortController().signal,
      ),
      /request body exceeds/,
    )

    await assert.rejects(
      () => dispatchPluginRoute(
        { owners },
        { owner: owner.ref, method: 'GET', path: '/huge', headers: [], deadline_ms: 5000 },
        new AbortController().signal,
      ),
      /response exceeded/,
    )
  } finally {
    clearOwnerRoutes('tok-cap')
  }
})

test('connection.fetch, connection.rpc, requestRejection, sessionTitle, and profileContext work', async () => {
  const { owners, makeOwner } = await createHarness()
  const { owner, ctx } = makeOwner('plugin-extras', 'tok-extras', '/tmp/plugin-extras')
  try {
    assert.equal(ctx.connection.isLoopback, true)
    assert.equal(ctx.connection.requestRejection({ headers: { host: '127.0.0.1:4319' } }), null)
    assert.equal(ctx.connection.requestRejection({ headers: { host: 'evil.example' } })?.status, 403)
    assert.equal(ctx.connection.requestRejection({ headers: { host: 'localhost', 'sec-fetch-site': 'cross-site' } })?.status, 403)

    ctx.connection.fetch.register({
      path: '/api/skills',
      methods: ['GET'],
      fetch: async (request) => new Response(JSON.stringify({ url: request.url }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    })
    const fetchRes = await dispatchPluginRoute(
      { owners },
      { owner: owner.ref, method: 'GET', path: '/api/skills', query: 'q=rust', headers: [], deadline_ms: 5000 },
      new AbortController().signal,
    )
    assert.equal(fetchRes.matched, true)
    assert.equal(fetchRes.status, 200)
    assert.equal(JSON.parse(Buffer.from(fetchRes.body_base64, 'base64').toString('utf8')).url, 'http://127.0.0.1/api/skills?q=rust')

    ctx.connection.rpc.handle('/arcv', (endpoint, payload) => ({ endpoint, count: payload.n + 1 }))
    const rpcRes = await dispatchPluginRoute(
      { owners },
      {
        owner: owner.ref,
        method: 'POST',
        path: '/arcv',
        headers: [{ name: 'content-type', value: 'application/json' }],
        body_base64: Buffer.from(JSON.stringify({ endpoint: 'list', payload: { n: 41 } }), 'utf8').toString('base64'),
        deadline_ms: 5000,
      },
      new AbortController().signal,
    )
    assert.equal(rpcRes.matched, true)
    assert.deepEqual(JSON.parse(Buffer.from(rpcRes.body_base64, 'base64').toString('utf8')), { endpoint: 'list', count: 42 })

    const observed = []
    const disposeTitle = ctx.sessionTitle.register((id, title) => observed.push([id, title]))
    ctx.sessionTitle.rename({ id: 'sess-1' }, '  My\nSession  ')
    assert.equal(ctx.sessionTitle.get('sess-1'), 'My Session')
    assert.deepEqual(observed, [['sess-1', 'My Session']])
    disposeTitle()

    assert.equal(ctx.profileContext.name, 'codewhale')
    assert.equal(ctx.profileContext.dir, '/tmp/plugin-extras')
    assert.equal(ctx.profileContext.installAnchor, '/tmp/plugin-extras')
  } finally {
    clearOwnerRoutes('tok-extras')
  }
})

test('profileContext fails closed without scoped dataDir, sessionTitle is owner-isolated, and fetch streams enforce 8 MiB cap', async () => {
  const { owners, makeOwner } = await createHarness()
  const a = makeOwner('plugin-sec-a', 'tok-sec-a', '/tmp/sec-a')
  const b = makeOwner('plugin-sec-b', 'tok-sec-b', undefined)
  try {
    assert.throws(() => b.ctx.profileContext.dir, /scoped dataDir/)
    assert.throws(() => b.ctx.profileContext.installAnchor, /scoped dataDir/)

    const bEvents = []
    const disposeB = b.ctx.sessionTitle.register((id, title) => bEvents.push([id, title]))
    a.ctx.sessionTitle.rename('sess-shared', 'Plugin A Private')
    assert.equal(a.ctx.sessionTitle.get('sess-shared'), 'Plugin A Private')
    assert.equal(b.ctx.sessionTitle.get('sess-shared'), undefined)
    assert.deepEqual(bEvents, [])
    disposeB()

    a.ctx.connection.fetch.register({
      path: '/api/stream-huge',
      methods: ['GET'],
      fetch: () => {
        const chunk = new Uint8Array(1024 * 1024)
        let sent = 0
        const stream = new ReadableStream({
          pull(controller) {
            if (sent >= 9) {
              controller.close()
              return
            }
            sent += 1
            controller.enqueue(chunk)
          },
        })
        return new Response(stream, { status: 200 })
      },
    })

    await assert.rejects(
      () => dispatchPluginRoute(
        { owners },
        { owner: a.owner.ref, method: 'GET', path: '/api/stream-huge', headers: [], deadline_ms: 5000 },
        new AbortController().signal,
      ),
      /response exceeded/,
    )
  } finally {
    clearOwnerRoutes('tok-sec-a')
    clearOwnerRoutes('tok-sec-b')
  }
})
