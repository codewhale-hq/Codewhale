import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'
import { Context } from '@deepseek-ai/cordis'
import { activate, FIXTURES, startHost } from './harness.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const PNG_1X1_B64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='
const PNG_1X1 = Buffer.from(PNG_1X1_B64, 'base64')
const PNG_ID = `sha256:${createHash('sha256').update(PNG_1X1).digest('hex')}`

let attachmentsAdapter
let AttachmentError
let isAttachmentError
let IMAGE_LIMITS
let ErrorCode
let RpcError
let RpcPeer
let HostRoot
let DSH_ADAPTERS
let dshAttachmentsPlugin

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-attachments-'))
  await build({
    entryPoints: {
      attachments: join(here, '../src/dsh/attachments.ts'),
      protocol: join(here, '../src/protocol.generated.ts'),
      rpc: join(here, '../src/rpc.ts'),
      root: join(here, '../src/root.ts'),
      adapters: join(here, '../src/dsh/adapters.ts'),
    },
    outdir: dir,
    outExtension: { '.js': '.mjs' },
    splitting: true,
    bundle: true,
    platform: 'node',
    format: 'esm',
    packages: 'external',
    absWorkingDir: join(here, '..'),
    logLevel: 'silent',
  })
  ;({ attachmentsAdapter, AttachmentError, isAttachmentError } = await import(pathToFileURL(join(dir, 'attachments.mjs')).href))
  ;({ IMAGE_LIMITS, ErrorCode } = await import(pathToFileURL(join(dir, 'protocol.mjs')).href))
  ;({ RpcError, RpcPeer } = await import(pathToFileURL(join(dir, 'rpc.mjs')).href))
  ;({ HostRoot } = await import(pathToFileURL(join(dir, 'root.mjs')).href))
  ;({ DSH_ADAPTERS } = await import(pathToFileURL(join(dir, 'adapters.mjs')).href))
  dshAttachmentsPlugin = await import(pathToFileURL(join(FIXTURES, 'dsh-attachments', 'index.mjs')).href)
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

function brokerHost(answers = {}) {
  const calls = []
  return {
    calls,
    host: {
      tier: 'plugin',
      broker: async (_ctx, service, method, input, signal) => {
        calls.push({ service, method, input, signal })
        const answer = answers[method]
        if (answer instanceof Error) throw answer
        return typeof answer === 'function' ? answer(input) : answer
      },
    },
  }
}

function makeService(answers) {
  const { host, calls } = brokerHost(answers)
  const Attachments = attachmentsAdapter.define(host)
  const service = Object.create(Attachments.prototype)
  service.ctx = {}
  return { service, calls, host }
}

test('imageLimits mirrors generated IMAGE_LIMITS and is frozen', () => {
  const { service } = makeService()
  assert.deepEqual(service.imageLimits, IMAGE_LIMITS)
  assert.equal(Object.isFrozen(service.imageLimits), true)
  assert.equal(Object.isFrozen(service.imageLimits.mediaTypes), true)
  assert.equal(service.imageHostPath({ attachmentId: PNG_ID }), undefined)
})

test('validateImage, saveImage, readImage, and readImageRequest match the broker contract', async () => {
  const variantId = `sha256:${'b'.repeat(64)}`
  const savedRef = {
    attachment_id: PNG_ID,
    media_type: 'image/png',
    bytes: PNG_1X1.byteLength,
    width: 1,
    height: 1,
  }
  const { service, calls } = makeService({
    validate: { ok: true, media_type: 'image/png', bytes: PNG_1X1.byteLength, width: 1, height: 1 },
    save: { attachment: savedRef },
    read: { data_base64: PNG_1X1_B64, attachment: savedRef },
    read_request: { variant_id: variantId, data_base64: PNG_1X1_B64, media_type: 'image/png' },
  })

  const validated = await service.validateImage({ data: PNG_1X1, mediaType: 'image/png', name: 'sample.png' })
  assert.deepEqual(validated, { ok: true, mediaType: 'image/png', bytes: PNG_1X1.byteLength, width: 1, height: 1 })
  await service.validateImage({ data: PNG_1X1, mediaType: 'image/png' })
  assert.equal('name' in calls[1].input, false)

  const saved = await service.saveImage({ data: PNG_1X1, mediaType: 'image/png', name: 'studio.png' })
  assert.deepEqual(saved, {
    attachmentId: PNG_ID,
    mediaType: 'image/png',
    bytes: PNG_1X1.byteLength,
    width: 1,
    height: 1,
  })

  const read = await service.readImage({ attachmentId: PNG_ID, mediaType: 'image/png' })
  assert.equal(read.data instanceof Uint8Array, true)
  assert.deepEqual(Buffer.from(read.data), PNG_1X1)
  assert.deepEqual(read.ref, saved)

  const variant = await service.readImageRequest(saved, { width: 320, height: 320, maxBytes: 98304 })
  assert.equal(variant.variantId, variantId)
  assert.equal(variant.mediaType, 'image/png')
  assert.deepEqual(Buffer.from(variant.data), PNG_1X1)

  assert.deepEqual(calls.map((c) => [c.service, c.method]), [
    ['attachments', 'validate'],
    ['attachments', 'validate'],
    ['attachments', 'save'],
    ['attachments', 'read'],
    ['attachments', 'read_request'],
  ])
  assert.equal(calls[0].input.data_base64, PNG_1X1_B64)
  assert.deepEqual(calls[4].input, {
    attachment: savedRef,
    width: 320,
    height: 320,
    max_bytes: 98304,
  })
})

test('AttachmentError preserves upstream codes and local validation rejects bad inputs before RPC', async () => {
  const { service, calls } = makeService({
    save: () => { throw new RpcError(ErrorCode.Refused, '[TOO_MANY_IMAGES] this Codewhale keeps one image per tool result') },
    read: () => { throw new RpcError(ErrorCode.Refused, '[ATTACHMENT_NOT_FOUND] attachment not found') },
    validate: () => { throw new RpcError(ErrorCode.Denied, 'attachments access denied') },
  })

  await assert.rejects(
    service.saveImage({ data: PNG_1X1, mediaType: 'image/png' }),
    (err) => isAttachmentError(err) && err instanceof AttachmentError && err.code === 'TOO_MANY_IMAGES',
  )
  await assert.rejects(
    service.readImage({ attachmentId: PNG_ID }),
    (err) => isAttachmentError(err) && err.code === 'ATTACHMENT_NOT_FOUND',
  )
  await assert.rejects(
    service.validateImage({ data: PNG_1X1, mediaType: 'image/png' }),
    (err) => err instanceof RpcError && err.code === ErrorCode.Denied && !isAttachmentError(err),
  )

  const beforeLocal = calls.length
  await assert.rejects(
    service.validateImage({ data: PNG_1X1, mediaType: 'image/svg+xml' }),
    (err) => isAttachmentError(err) && err.code === 'UNSUPPORTED_IMAGE_TYPE',
  )
  await assert.rejects(
    service.saveImage({ data: new Uint8Array(0), mediaType: 'image/png' }),
    (err) => isAttachmentError(err) && err.code === 'INVALID_IMAGE',
  )
  await assert.rejects(
    service.saveImage({ data: new Uint8Array(IMAGE_LIMITS.maxImageBytes + 1), mediaType: 'image/png' }),
    (err) => isAttachmentError(err) && err.code === 'IMAGE_TOO_LARGE',
  )
  await assert.rejects(
    service.readImage({ attachmentId: 'bad-id' }),
    (err) => isAttachmentError(err) && err.code === 'INVALID_ATTACHMENT_REF',
  )
  await assert.rejects(
    service.saveImage({ data: PNG_1X1, mediaType: 'image/png', name: '../escape.png' }),
    (err) => err instanceof RpcError && err.code === ErrorCode.InvalidParams,
  )
  await assert.rejects(
    service.readImage({ attachmentId: PNG_ID, hostPath: '/etc/passwd' }),
    (err) => err instanceof RpcError && err.code === ErrorCode.InvalidParams,
  )
  await assert.rejects(
    service.readImageRequest({ attachmentId: PNG_ID }, { width: IMAGE_LIMITS.maxImageDimension + 1 }),
    (err) => isAttachmentError(err) && err.code === 'IMAGE_DIMENSION_TOO_LARGE',
  )
  await assert.rejects(
    service.readImageRequest({ attachmentId: PNG_ID }, { maxBytes: IMAGE_LIMITS.maxImageBytes + 1 }),
    (err) => isAttachmentError(err) && err.code === 'IMAGE_TOO_LARGE',
  )
  assert.equal(calls.length, beforeLocal, 'locally invalid requests must never call the broker')
})

test('AbortSignal aborts before or during a broker call and generic file methods fail NotAvailable', async () => {
  let releasePending
  const { service, calls } = makeService({
    read: () => new Promise((resolve) => { releasePending = resolve }),
  })

  const alreadyAborted = new AbortController()
  alreadyAborted.abort()
  await assert.rejects(
    service.readImage({ attachmentId: PNG_ID }, alreadyAborted.signal),
    (err) => isAttachmentError(err) && err.code === 'CANCELLED',
  )
  assert.throws(
    () => service.imageHostPath({ attachmentId: PNG_ID }, alreadyAborted.signal),
    (err) => isAttachmentError(err) && err.code === 'CANCELLED',
  )
  assert.equal(calls.length, 0)

  const inFlight = new AbortController()
  const pending = service.readImage({ attachmentId: PNG_ID }, inFlight.signal)
  assert.equal(calls.length, 1)
  assert.equal(calls[0].signal, inFlight.signal)
  assert.equal(calls[0].signal.aborted, false)
  inFlight.abort()
  assert.equal(calls[0].signal.aborted, true)
  await assert.rejects(pending, (err) => isAttachmentError(err) && err.code === 'CANCELLED')
  releasePending?.({ data_base64: PNG_1X1_B64, attachment: { attachment_id: PNG_ID, media_type: 'image/png', bytes: PNG_1X1.byteLength, width: 1, height: 1 } })

  for (const method of ['saveFile', 'admitEncodedFile', 'saveFileStream', 'readFileStream', 'readFile', 'fileHostPath']) {
    assert.throws(() => service[method](), (err) => err instanceof RpcError && err.code === ErrorCode.NotAvailable, method)
  }
})

test('consumer-shaped fixture executes against attachmentsAdapter inside Cordis and renders image attachment blocks', async (t) => {
  const savedRef = {
    attachment_id: PNG_ID,
    media_type: 'image/png',
    bytes: PNG_1X1.byteLength,
    width: 1,
    height: 1,
  }
  const variantId = `sha256:${'c'.repeat(64)}`
  const { host, calls } = brokerHost({
    read: { data_base64: PNG_1X1_B64, attachment: savedRef },
    validate: { ok: true, media_type: 'image/png', bytes: PNG_1X1.byteLength, width: 1, height: 1 },
    save: { attachment: savedRef },
    read_request: { variant_id: variantId, data_base64: PNG_1X1_B64, media_type: 'image/png' },
  })

  const ctx = new Context()
  const tools = new Map()
  ctx.provide('tools', { register(spec) { tools.set(spec.name, spec) } })
  const adapterFiber = ctx.plugin(attachmentsAdapter.define(host))
  await adapterFiber.await()
  const pluginFiber = ctx.plugin(dshAttachmentsPlugin)
  await pluginFiber.await()
  t.after(async () => {
    await pluginFiber.dispose()
    await adapterFiber.dispose()
  })

  const tool = tools.get('attachments_roundtrip')
  assert.ok(tool)
  const controller = new AbortController()
  const input = {
    sourceRef: { attachmentId: PNG_ID, mediaType: 'image/png' },
    saveBase64: PNG_1X1_B64,
    mediaType: 'image/png',
    imageName: 'studio.png',
  }
  const value = await tool.execute(input, { signal: controller.signal })
  const rendered = tool.output.render(input, value)
  assert.deepEqual(rendered, [
    { type: 'text', text: `read=${PNG_1X1.byteLength} saved=${PNG_ID} thumb=${PNG_1X1.byteLength}` },
    { type: 'image', attachment: value.attachment },
  ])
  assert.deepEqual(calls.map((c) => c.method), ['read', 'validate', 'save', 'read_request'])
})

test('real host refuses dsh-attachments activation until DSH-16 turn-on and projects renderResult image blocks', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())

  const unoffered = await activate(host, 'dsh-attachments')
  assert.equal(unoffered.result.status, 'failed')
  assert.match(unoffered.result.diagnostic, /requires `attachments`/)

  const renderActivated = await activate(host, 'dsh-attachments-render')
  assert.equal(renderActivated.result.status, 'ok', renderActivated.result.diagnostic)
  const reg = host.registry.find((entry) => entry.op === 'register' && entry.spec.name === 'render_attachment_result')
  assert.ok(reg)

  const out = await host.call('tool/call', {
    handle: reg.handle,
    call_id: 'render-1',
    input: { attachmentId: PNG_ID },
    deadline_ms: 5000,
  })
  assert.equal(out.is_error, false)
  assert.deepEqual(out.structured, { attachmentId: PNG_ID })
  assert.deepEqual(out.content, [
    { type: 'text', text: `image ${PNG_ID}` },
    { type: 'image_ref', attachment_id: PNG_ID },
  ])
})

test('HostRoot adapterHost.broker forwards per-call AbortSignal into rpc.request and sends $/cancel', async (t) => {
  const sent = []
  let peer
  let handleSeq = 1
  const brokerSeen = new Promise((resolve) => {
    peer = new RpcPeer((message) => {
      sent.push(message)
      if (message.method === 'registry/register') {
        const handle = handleSeq++
        queueMicrotask(() => peer.handle({ jsonrpc: '2.0', id: message.id, result: { handle } }))
      } else if (message.method === 'registry/unregister') {
        queueMicrotask(() => peer.handle({ jsonrpc: '2.0', id: message.id, result: {} }))
      } else if (message.method === 'dsh/broker') {
        resolve(message)
      }
    }, 'plugin')
  })

  const callAbort = new AbortController()
  const baseRemote = DSH_ADAPTERS.find((adapter) => adapter.service === 'remote')
  let capturedHost
  const cancelProbeRemote = {
    service: 'remote',
    define(adapterHost) {
      capturedHost = adapterHost
      const BaseRemote = baseRemote.define(adapterHost)
      return class extends BaseRemote {
        statusWithSignal(signal) {
          return adapterHost.broker(this.ctx, 'remote', 'status', {}, signal)
        }
      }
    },
  }
  const root = new HostRoot(
    peer,
    'plugin',
    [...DSH_ADAPTERS.filter((adapter) => adapter.service !== 'remote'), cancelProbeRemote],
  )

  const dir = mkdtempSync(join(tmpdir(), 'cw-broker-abort-'))
  t.after(() => rmSync(dir, { recursive: true, force: true }))
  const entry = join(dir, 'index.mjs')
  writeFileSync(
    entry,
    [
      "export const name = 'abort-probe'",
      "export const inject = ['tools', 'remote']",
      'export function apply(ctx) {',
      '  ctx.tools.register({',
      "    name: 'probe_broker_abort',",
      "    parameters: { type: 'object' },",
      '    async execute(_input) {',
      '      await ctx.remote.statusWithSignal(globalThis.__cwTestPerCallAbort.signal)',
      "      return 'done'",
      '    },',
      '  })',
      '}',
    ].join('\n'),
  )
  globalThis.__cwTestPerCallAbort = callAbort
  t.after(() => { delete globalThis.__cwTestPerCallAbort })

  const owner = { plugin_id: 'abort-probe', generation: 1, owner_token: 'tok-abort-probe' }
  const activated = await root.activate({
    owner,
    plugin_name: 'abort-probe',
    entry: { kind: 'native', path: entry, sha256: createHash('sha256').update(readFileSync(entry)).digest('hex') },
    config: {},
  })
  assert.equal(activated.status, 'ok', activated.diagnostic)
  const regMsg = sent.find((m) => m.method === 'registry/register')
  assert.ok(regMsg)
  const registeredHandle = 1

  const invocationAbort = new AbortController()
  const toolPromise = root.callTool(
    registeredHandle,
    {},
    'call-abort-1',
    invocationAbort.signal,
    dir,
    'core-tkt',
    { session_id: 'sess-abort', broker_tickets: [{ service: 'remote', ticket: 'remote-tkt' }] },
  )
  const brokerReq = await brokerSeen
  assert.equal(brokerReq.params.service, 'remote')
  assert.equal(brokerReq.params.method, 'status')

  callAbort.abort()
  await assert.rejects(toolPromise, (err) => err instanceof RpcError && err.code === ErrorCode.Cancelled)
  const cancelMsg = sent.find((m) => m.method === '$/cancel')
  assert.deepEqual(cancelMsg, { jsonrpc: '2.0', method: '$/cancel', params: { id: brokerReq.id } })
  assert.equal(invocationAbort.signal.aborted, false)

  await assert.rejects(
    capturedHost.broker({}, 'remote', 'status', {}, 'not-a-signal'),
    (err) => err instanceof RpcError && err.code === ErrorCode.InvalidParams,
  )
  await root.deactivateAll(1000)
})
