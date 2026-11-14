import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { Context } from '@deepseek-ai/cordis'
import { build } from 'esbuild'
import { activate, startHost, FIXTURES } from './harness.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const hash = (value) => createHash('sha256').update(value).digest('hex')

let tempBuildDir
let cotModule
let compositionModule
let networkModule

before(async () => {
  tempBuildDir = mkdtempSync(join(here, '..', 'node_modules', '.cw-cot-translation-'))
  const buildModule = async (entry, outName) => {
    const outfile = join(tempBuildDir, outName)
    await build({
      entryPoints: [join(here, entry)],
      outfile,
      bundle: true,
      platform: 'node',
      format: 'esm',
      packages: 'external',
      absWorkingDir: join(here, '..'),
      logLevel: 'silent',
    })
    return import(pathToFileURL(outfile).href)
  }
  ;[cotModule, compositionModule, networkModule] = await Promise.all([
    buildModule('../src/dsh/cot-translation.ts', 'cot-translation.mjs'),
    buildModule('../src/dsh/composition.ts', 'composition.mjs'),
    buildModule('../src/dsh/network.ts', 'network.mjs'),
  ])
})

after(() => {
  if (tempBuildDir) rmSync(tempBuildDir, { recursive: true, force: true })
})

function temporaryDir(t) {
  const dir = mkdtempSync(join(tmpdir(), 'cw-cot-test-'))
  t.after(() => rmSync(dir, { recursive: true, force: true }))
  return dir
}

function brokerHost(responder) {
  const calls = []
  return {
    calls,
    host: {
      tier: 'plugin',
      broker: async (_ctx, service, method, input) => {
        calls.push({ service, method, input })
        return responder(input, { service, method, calls })
      },
    },
  }
}

async function mountTranslator(t, responder, config = {}) {
  const { host, calls } = brokerHost(responder)
  const Credentials = networkModule.credentialsAdapter.define(host)
  const ctx = new Context()
  t.after(() => ctx.fiber.dispose())
  const llmCalls = []
  ctx.provide('llm')
  ctx.set('llm', {
    complete: async (input) => {
      llmCalls.push(input)
      throw new Error('llm.complete must never be called by cot-translation bridge')
    },
  })
  await ctx.plugin(Credentials)
  await ctx.plugin(cotModule.reviewedCotTranslationModule(), config)
  return { ctx, translator: ctx.translator, calls, llmCalls }
}

async function mountTranslatorWithFetch(t, fetchImpl, config = {}) {
  const ctx = new Context()
  t.after(() => ctx.fiber.dispose())
  ctx.provide('credentials')
  ctx.set('credentials', { fetch: fetchImpl })
  ctx.provide('llm')
  ctx.set('llm', {
    complete: async () => {
      throw new Error('llm.complete must never be called by cot-translation bridge')
    },
  })
  await ctx.plugin(cotModule.reviewedCotTranslationModule(), config)
  return { ctx, translator: ctx.translator }
}

function jsonBrokerResponse(status, payload, url = 'https://example.invalid/translate') {
  return {
    status,
    url,
    redirected: false,
    headers: [['content-type', 'application/json']],
    body_base64: Buffer.from(JSON.stringify(payload), 'utf8').toString('base64'),
  }
}

test('the bundle installed but off sends nothing', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const dir = temporaryDir(t)
  const patchPath = join(FIXTURES, 'cot-translation', 'cordis.patch.yml')
  const patchSource = readFileSync(patchPath, 'utf8')
  const disabledLayer = patchSource.replace(
    /^(\s*-\s*id:\s*\S+)/gm,
    '$1\n      disabled: true',
  )
  const spec = {
    version: 1,
    layers: [{ path: 'cordis.patch.yml', sha256: hash(disabledLayer), source: disabledLayer }],
    modules: [],
    files: {},
  }
  writeFileSync(join(dir, 'composition.json'), JSON.stringify(spec))
  const entry = join(dir, 'index.mjs')
  writeFileSync(
    entry,
    `import { mountReviewedComposition } from '@codewhale/dsh-composition'; import spec from './composition.json' with { type: 'json' }; export async function apply(ctx) { await mountReviewedComposition(ctx, new URL('./', import.meta.url).href, spec) }`,
  )
  const { ref, result } = await activate(host, 'cot-translation-off', entry, { data_dir: dir })
  assert.equal(result.status, 'ok', result.diagnostic)
  assert.deepEqual(host.coreCalls, [])
  assert.deepEqual(host.registry, [])
  assert.deepEqual(await host.call('ext/deactivate', { owner: ref }), { disposed: true, leaked: [] })
})

test('without its row_bridges entry no package resolves to the bridge', async (t) => {
  const pkg = '@deepseek-ai/dsh-experimental-translator'
  assert.equal(compositionModule.hasReviewedRowBridge(pkg), false)
  assert.equal(compositionModule.hasReviewedRowBridge('__proto__'), false)
  assert.equal(compositionModule.hasReviewedRowBridge('constructor'), false)

  const ctx = new Context()
  compositionModule.installCompositionLoader(ctx)
  t.after(() => ctx.fiber.dispose())
  for (const candidate of [pkg, 'toString']) {
    const layer = `- insert:\n    - id: check\n      name: ${JSON.stringify(candidate)}\n`
    await assert.rejects(
      compositionModule.mountReviewedComposition(
        ctx,
        new URL('./', pathToFileURL(join(FIXTURES, 'cot-translation', 'package.json'))).href,
        { version: 1, layers: [{ path: 'cordis.patch.yml', sha256: hash(layer), source: layer }], modules: [], files: {} },
      ),
      /composition row "check" failed activation|absent from the reviewed module closure/,
    )
  }

  const host = await startHost()
  t.after(() => host.stop())
  const dir = temporaryDir(t)
  const enabledLayer = [
    '- insert:',
    '    - id: translator',
    `      name: ${JSON.stringify(pkg)}`,
    '      config: {}',
  ].join('\n')
  const spec = {
    version: 1,
    layers: [{ path: 'cordis.patch.yml', sha256: hash(enabledLayer), source: enabledLayer }],
    modules: [],
    files: {},
  }
  writeFileSync(join(dir, 'composition.json'), JSON.stringify(spec))
  const entry = join(dir, 'index.mjs')
  writeFileSync(
    entry,
    `import { mountReviewedComposition } from '@codewhale/dsh-composition'; import spec from './composition.json' with { type: 'json' }; export async function apply(ctx) { await mountReviewedComposition(ctx, new URL('./', import.meta.url).href, spec) }`,
  )
  const { result } = await activate(host, 'cot-translation-unbridged', entry, { data_dir: dir })
  assert.equal(result.status, 'failed')
  assert.match(
    result.diagnostic,
    /composition row "translator" failed activation|absent from the reviewed module closure/,
  )
  assert.deepEqual(host.coreCalls, [])
})

test('bing and google requests go only through the network broker', async (t) => {
  const originalFetch = globalThis.fetch
  globalThis.fetch = async () => {
    throw new Error('global fetch must never be called')
  }
  t.after(() => {
    globalThis.fetch = originalFetch
  })

  const bingFixture = JSON.parse(
    readFileSync(join(FIXTURES, 'cot-translation', 'bing-response.json'), 'utf8'),
  )
  const googleFixture = [[['Hello from Google', '你好世界', null, null, 1]], null, 'zh-CN']

  const { translator, calls, llmCalls } = await mountTranslator(t, (input) => {
    const parsedUrl = new URL(input.url)
    if (parsedUrl.hostname === 'edge.microsoft.com') {
      return jsonBrokerResponse(200, bingFixture, input.url)
    }
    if (parsedUrl.hostname === 'translate.googleapis.com') {
      return jsonBrokerResponse(200, googleFixture, input.url)
    }
    throw new Error(`unexpected broker url: ${input.url}`)
  })

  assert.deepEqual(await translator.availableProviders(), ['bing', 'google'])
  assert.equal(calls.length, 0)

  const bingSpec = translator.resolve({
    text: '思考过程',
    sourceLanguage: 'zh-CN',
    targetLanguage: 'en',
    provider: 'bing',
    sessionId: 'sess-123',
  })
  assert.equal(bingSpec.sessionId, 'sess-123')
  const bingOut = await translator.translate(bingSpec)
  assert.equal(bingOut, 'Hello from Bing')

  const googleSpec = translator.resolve({
    text: '你好世界',
    sourceLanguage: 'zh-Hans',
    targetLanguage: 'zh-Hant',
    provider: 'google',
  })
  const googleOut = await translator.translate(googleSpec)
  assert.equal(googleOut, 'Hello from Google')

  assert.equal(calls.length, 2)
  assert.deepEqual(calls.map((c) => [c.service, c.method]), [
    ['network', 'fetch'],
    ['network', 'fetch'],
  ])
  assert.equal(
    calls[0].input.url,
    'https://edge.microsoft.com/translate/translatetext?from=zh-Hans&to=en&isEnterpriseClient=false',
  )
  assert.equal(calls[0].input.method, 'POST')
  assert.equal(
    Buffer.from(calls[0].input.body_base64, 'base64').toString('utf8'),
    JSON.stringify(['思考过程']),
  )

  assert.equal(
    calls[1].input.url,
    'https://translate.googleapis.com/translate_a/single',
  )
  assert.equal(calls[1].input.method, 'POST')
  assert.equal(
    Buffer.from(calls[1].input.body_base64, 'base64').toString('utf8'),
    'client=gtx&sl=zh-CN&tl=zh-TW&dt=t&q=%E4%BD%A0%E5%A5%BD%E4%B8%96%E7%95%8C',
  )
  assert.deepEqual(llmCalls, [])
})

test('a translate call with no ticket is refused and sends nothing', async (t) => {
  const originalFetch = globalThis.fetch
  let globalFetchCount = 0
  globalThis.fetch = async () => {
    globalFetchCount += 1
    throw new Error('global fetch must never be called')
  }
  t.after(() => {
    globalThis.fetch = originalFetch
  })

  const { translator, calls } = await mountTranslator(t, () => {
    const err = new Error('network broker call requires an active ticket')
    throw err
  })

  const spec = translator.resolve({
    text: 'private reasoning content',
    targetLanguage: 'en',
    provider: 'bing',
  })
  await assert.rejects(translator.translate(spec), (err) => {
    assert.equal(err instanceof cotModule.TranslationError, true)
    assert.equal(err.code, 'TRANSLATION_REQUEST_FAILED')
    assert.equal(err.message.includes('private reasoning content'), false)
    return true
  })
  assert.equal(calls.length, 1)
  assert.equal(globalFetchCount, 0)

  const host = await startHost()
  t.after(() => host.stop())
  const dir = temporaryDir(t)
  const entry = join(dir, 'require-services.mjs')
  const required = cotModule.reviewedCotTranslationModule().inject
  assert.deepEqual(required, ['credentials', 'llm'])
  writeFileSync(
    entry,
    `export const inject = ${JSON.stringify(required)}; export function apply() {}`,
  )
  const { result } = await activate(host, 'cot-translation-no-grants', entry, { data_dir: dir })
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /credentials|llm|refused|missing/i)
  assert.deepEqual(host.coreCalls, [])
})

test('a refused send answers TRANSLATION_REQUEST_FAILED and keeps no text', async (t) => {
  const secretSource = 'CONFIDENTIAL_COT_TEXT_ABC'
  const leakedUpstreamBody = 'UPSTREAM_BODY_TOKEN_XYZ'
  const { translator } = await mountTranslator(t, () => {
    throw new Error(`broker rejected: ${secretSource} :: ${leakedUpstreamBody}`)
  })

  const spec = translator.resolve({
    text: secretSource,
    targetLanguage: 'en',
    provider: 'bing',
  })
  await assert.rejects(translator.translate(spec), (err) => {
    assert.equal(err instanceof cotModule.TranslationError, true)
    assert.equal(err.code, 'TRANSLATION_REQUEST_FAILED')
    assert.equal(err.message.includes(secretSource), false)
    assert.equal(err.message.includes(leakedUpstreamBody), false)
    return true
  })

  const { translator: httpTranslator } = await mountTranslator(t, () => ({
    status: 502,
    url: 'https://edge.microsoft.com/translate/translatetext',
    redirected: false,
    headers: [['content-type', 'text/plain']],
    body_base64: Buffer.from(leakedUpstreamBody, 'utf8').toString('base64'),
  }))
  await assert.rejects(
    httpTranslator.translate({
      text: secretSource,
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) => {
      assert.equal(err.code, 'TRANSLATION_HTTP_ERROR')
      assert.equal(err.message.includes(secretSource), false)
      assert.equal(err.message.includes(leakedUpstreamBody), false)
      return true
    },
  )

  await assert.rejects(
    cotModule.cotTranslationTranslate(
      translator,
      { provider: 'bing', targetLanguage: 'en' },
      { text: secretSource, targetLanguage: 'en', provider: 'bing' },
    ),
    (err) => {
      assert.equal(err.code, 'cotTranslation/failed')
      assert.deepEqual(err.data, {})
      assert.equal(err.message, 'Translation failed')
      assert.equal(err.message.includes(secretSource), false)
      assert.equal(err.message.includes(leakedUpstreamBody), false)
      return true
    },
  )
})

test('an over-limit fragment is refused before any send', async (t) => {
  const { translator, calls } = await mountTranslator(
    t,
    () => {
      throw new Error('broker must not be reached for over-limit input')
    },
    { maxTextChars: 10 },
  )
  assert.equal(translator.maxTextChars, 10)
  const tooLong = '0123456789a'
  assert.throws(
    () => translator.resolve({ text: tooLong, targetLanguage: 'en', provider: 'bing' }),
    (err) => {
      assert.equal(err.code, 'TRANSLATION_TEXT_LIMIT')
      assert.equal(err.message.includes(tooLong), false)
      return true
    },
  )
  await assert.rejects(
    translator.translate({
      text: tooLong,
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) => {
      assert.equal(err.code, 'TRANSLATION_TEXT_LIMIT')
      assert.equal(err.message.includes(tooLong), false)
      return true
    },
  )
  assert.equal(
    await translator.translate({
      text: '',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    '',
  )
  assert.equal(calls.length, 0)
})

test("paid routes follow the design page's rule", async (t) => {
  const { translator, calls, llmCalls } = await mountTranslator(t, () => {
    throw new Error('paid routes must never reach the network broker')
  })

  const available = await translator.availableProviders()
  assert.deepEqual(available, ['bing', 'google'])
  assert.equal(available.includes('deepseek-account'), false)
  assert.equal(available.includes('deepseek-official'), false)

  for (const provider of ['deepseek-account', 'deepseek-official']) {
    const spec = translator.resolve({
      text: 'reason carefully',
      targetLanguage: 'en',
      provider,
    })
    assert.equal(spec.provider, provider)
    await assert.rejects(translator.translate(spec), (err) => {
      assert.equal(err instanceof cotModule.TranslationError, true)
      assert.equal(err.code, 'TRANSLATION_UNAVAILABLE')
      return true
    })
  }
  assert.equal(calls.length, 0)
  assert.deepEqual(llmCalls, [])
})

test('cancelling a translation aborts its broker call', async (t) => {
  const { translator, calls } = await mountTranslator(t, () => new Promise(() => {}))

  const controller = new AbortController()
  const reason = new Error('user cancelled translation')
  const pending = translator.translate(
    {
      text: 'cancel this request',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    },
    controller.signal,
  )
  await new Promise((resolve) => setTimeout(resolve, 10))
  assert.equal(calls.length, 1)
  controller.abort(reason)
  await assert.rejects(pending, (err) => err === reason)

  const preAborted = new AbortController()
  preAborted.abort(reason)
  await assert.rejects(
    cotModule.cotTranslationTranslate(
      translator,
      { provider: 'bing', targetLanguage: 'en' },
      { text: 'never sent', targetLanguage: 'en', provider: 'bing' },
      preAborted.signal,
    ),
    (err) => err === reason,
  )
  assert.equal(calls.length, 1)

  const { translator: timeoutTranslator } = await mountTranslator(
    t,
    () => new Promise(() => {}),
    { timeoutMs: 15 },
  )
  await assert.rejects(
    timeoutTranslator.translate({
      text: 'slow upstream',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) => err.code === 'TRANSLATION_TIMEOUT',
  )
})

test('the host half accepts only the selected provider and language', async (t) => {
  const bingFixture = JSON.parse(
    readFileSync(join(FIXTURES, 'cot-translation', 'bing-response.json'), 'utf8'),
  )
  const { translator, calls } = await mountTranslator(t, (input) =>
    jsonBrokerResponse(200, bingFixture, input.url),
  )

  const config = { provider: 'bing', targetLanguage: 'en' }
  const limits = await cotModule.cotTranslationLimits(translator, config)
  assert.deepEqual(limits, {
    maxTextChars: 4000,
    preferences: { provider: 'bing', targetLanguage: 'en' },
    availableProviders: ['bing', 'google'],
  })

  await assert.rejects(
    cotModule.cotTranslationTranslate(translator, config, {
      text: 'bonjour',
      targetLanguage: 'en',
      provider: 'google',
    }),
    (err) => err.code === 'cotTranslation/failed',
  )

  await assert.rejects(
    cotModule.cotTranslationTranslate(translator, config, {
      text: 'bonjour',
      targetLanguage: 'fr',
      provider: 'bing',
    }),
    (err) => err.code === 'cotTranslation/failed',
  )
  assert.equal(calls.length, 0)

  const translated = await cotModule.cotTranslationTranslate(translator, config, {
    text: '你好',
    targetLanguage: 'en',
  })
  assert.equal(translated, 'Hello from Bing')
  assert.equal(calls.length, 1)

  const autoLanguageTranslated = await cotModule.cotTranslationTranslate(
    translator,
    { provider: 'bing', targetLanguage: 'auto' },
    { text: '你好', targetLanguage: 'ja', provider: 'bing' },
  )
  assert.equal(autoLanguageTranslated, 'Hello from Bing')
  assert.equal(calls.length, 2)
})

test('committed cot-translation fixture carries expected metadata and LF endings', () => {
  const dir = join(FIXTURES, 'cot-translation')
  const manifest = JSON.parse(readFileSync(join(dir, 'tarballs-manifest.json'), 'utf8'))
  assert.equal(manifest.tarballs.length, 3)
  for (const name of [
    '.gitattributes',
    'package.json',
    'cordis.patch.yml',
    'LICENSE',
    'tarballs-manifest.json',
    'bing-response.json',
  ]) {
    const raw = readFileSync(join(dir, name), 'utf8')
    assert.equal(raw.includes('\r\n'), false, `${name} must use LF line endings`)
  }
})

test('endpoint validation, schemas, resolve validation, redirects, stream cleanup, and disposal fail closed', async (t) => {
  for (const bad of [
    'ftp://example.com/translate',
    'https://user:pass@example.com/translate',
    'https://example.com/translate#frag',
  ]) {
    assert.throws(
      () => cotModule.TranslatorConfigSchema({ bingEndpoint: bad }),
      /HTTP\(S\) URL without credentials or a fragment/,
    )
  }

  for (const allowed of [
    'http://example.com/translate',
    'https://example.com/translate?foo=bar',
  ]) {
    assert.equal(
      cotModule.TranslatorConfigSchema({ bingEndpoint: allowed }).bingEndpoint,
      allowed,
    )
  }

  const httpEndpoint = 'http://edge.microsoft.com/translate/translatetext?region=global'
  const { translator: unreviewedHttpTranslator, calls: httpEndpointCalls } =
    await mountTranslator(
      t,
      () => {
        throw new Error('unreviewed_site')
      },
      { bingEndpoint: httpEndpoint },
    )
  await assert.rejects(
    unreviewedHttpTranslator.translate({
      text: 'sensitive http text',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) =>
      err instanceof cotModule.TranslationError &&
      err.code === 'TRANSLATION_REQUEST_FAILED' &&
      !err.message.includes('sensitive http text'),
  )
  assert.equal(httpEndpointCalls.length, 1)
  assert.equal(
    httpEndpointCalls[0].input.url,
    'http://edge.microsoft.com/translate/translatetext?region=global&to=en&isEnterpriseClient=false',
  )

  assert.throws(() => cotModule.TranslatorConfigSchema({ maxTextChars: 1048577 }))
  assert.throws(() => cotModule.TranslatorConfigSchema({ maxResponseBytes: 8 * 1024 * 1024 + 1 }))

  for (const badLang of ['', 'en us', 'zh_CN']) {
    assert.throws(() => cotModule.CotTranslationConfigSchema({ targetLanguage: badLang }))
  }
  assert.equal(cotModule.CotTranslationConfigSchema({ targetLanguage: 'zh-Hans' }).targetLanguage, 'zh-Hans')

  const { translator: validatorTranslator } = await mountTranslator(t, () => {
    throw new Error('validation should fail before network')
  })
  for (const badReq of [
    { text: 'hi', targetLanguage: 'en', sourceLanguage: 42 },
    { text: 'hi', targetLanguage: 'en', sourceLanguage: '   ' },
    { text: 'hi', targetLanguage: 'en', sessionId: 99 },
    { text: 'hi', targetLanguage: 'en', sessionId: '   ' },
  ]) {
    assert.throws(
      () => validatorTranslator.resolve(badReq),
      (err) => err instanceof cotModule.TranslationError && err.code === 'TRANSLATION_INVALID_RESPONSE',
    )
  }

  const isolateCtx = new Context()
  compositionModule.installCompositionLoader(isolateCtx)
  t.after(() => isolateCtx.fiber.dispose())
  for (const isolateValue of [{ translator: 'shared' }, { translator: true }]) {
    const badLayer = `- insert:\n    - id: rogue\n      name: ./rogue.mjs\n      isolate: ${JSON.stringify(isolateValue)}\n`
    await assert.rejects(
      compositionModule.mountReviewedComposition(
        isolateCtx,
        new URL('./', pathToFileURL(join(FIXTURES, 'cot-translation', 'package.json'))).href,
        {
          version: 1,
          layers: [{ path: 'cordis.patch.yml', sha256: hash(badLayer), source: badLayer }],
          modules: [],
          files: {},
        },
      ),
      /cannot override translator isolation/,
    )
  }

  const { translator: redirectTranslator } = await mountTranslator(t, () => ({
    status: 200,
    url: 'https://evil.example/bounce',
    redirected: true,
    headers: [['content-type', 'application/json']],
    body_base64: Buffer.from('[]', 'utf8').toString('base64'),
  }))
  await assert.rejects(
    redirectTranslator.translate({
      text: 'hello',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) => err instanceof cotModule.TranslationError && err.code === 'TRANSLATION_REQUEST_FAILED',
  )

  const { translator: oversized } = await mountTranslator(
    t,
    () => jsonBrokerResponse(200, { huge: 'x'.repeat(200) }),
    { maxResponseBytes: 32 },
  )
  await assert.rejects(
    oversized.translate({
      text: 'hello',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) => err.code === 'TRANSLATION_RESPONSE_LIMIT',
  )

  const { translator: badJson } = await mountTranslator(t, () => ({
    status: 200,
    url: 'https://edge.microsoft.com/translate/translatetext',
    redirected: false,
    headers: [['content-type', 'application/json']],
    body_base64: Buffer.from('{not-json', 'utf8').toString('base64'),
  }))
  await assert.rejects(
    badJson.translate({
      text: 'hello',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) => err.code === 'TRANSLATION_INVALID_RESPONSE',
  )

  const trackedReaderResponse = (behavior) => {
    const state = { cancelCalls: 0, releaseCalls: 0 }
    const response = {
      ok: true,
      status: 200,
      redirected: false,
      body: {
        getReader() {
          if (behavior === 'locked') throw new TypeError('ReadableStream is locked')
          return {
            read() {
              if (behavior === 'error') return Promise.reject(new Error('stream broken'))
              return new Promise(() => {})
            },
            cancel() {
              state.cancelCalls += 1
              return Promise.resolve()
            },
            releaseLock() {
              state.releaseCalls += 1
            },
          }
        },
        cancel() {
          state.cancelCalls += 1
          return Promise.resolve()
        },
      },
    }
    return { response, state }
  }

  const abortDouble = trackedReaderResponse('hang')
  const { translator: streamAbortTranslator } = await mountTranslatorWithFetch(
    t,
    async () => abortDouble.response,
  )
  const abortController = new AbortController()
  const streamReason = new Error('abort stream')
  const abortRun = streamAbortTranslator.translate(
    { text: 'hi', sourceLanguage: 'auto', targetLanguage: 'en', provider: 'bing' },
    abortController.signal,
  )
  await new Promise((resolve) => setTimeout(resolve, 10))
  abortController.abort(streamReason)
  await assert.rejects(abortRun, (err) => err === streamReason)
  assert.equal(abortDouble.state.cancelCalls, 1)
  assert.equal(abortDouble.state.releaseCalls, 1)

  const timeoutDouble = trackedReaderResponse('hang')
  const { translator: streamTimeoutTranslator } = await mountTranslatorWithFetch(
    t,
    async () => timeoutDouble.response,
    { timeoutMs: 15 },
  )
  await assert.rejects(
    streamTimeoutTranslator.translate({
      text: 'hi',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) => err instanceof cotModule.TranslationError && err.code === 'TRANSLATION_TIMEOUT',
  )
  assert.equal(timeoutDouble.state.cancelCalls, 1)
  assert.equal(timeoutDouble.state.releaseCalls, 1)

  const errorDouble = trackedReaderResponse('error')
  const { translator: streamErrorTranslator } = await mountTranslatorWithFetch(
    t,
    async () => errorDouble.response,
  )
  await assert.rejects(
    streamErrorTranslator.translate({
      text: 'hi',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) => err instanceof cotModule.TranslationError && err.code === 'TRANSLATION_REQUEST_FAILED',
  )
  assert.equal(errorDouble.state.cancelCalls, 1)
  assert.equal(errorDouble.state.releaseCalls, 1)

  const lockedDouble = trackedReaderResponse('locked')
  const { translator: lockedTranslator } = await mountTranslatorWithFetch(
    t,
    async () => lockedDouble.response,
  )
  await assert.rejects(
    lockedTranslator.translate({
      text: 'hi',
      sourceLanguage: 'auto',
      targetLanguage: 'en',
      provider: 'bing',
    }),
    (err) => err instanceof cotModule.TranslationError && err.code === 'TRANSLATION_REQUEST_FAILED',
  )

  const { ctx: disposeCancelCtx, translator: disposeCancelTranslator } =
    await mountTranslatorWithFetch(t, async () => ({
      ok: false,
      status: 503,
      redirected: false,
      body: {
        cancel() {
          return new Promise(() => {})
        },
      },
    }))
  const failingHttp = disposeCancelTranslator.translate({
    text: 'hanging cancel',
    sourceLanguage: 'auto',
    targetLanguage: 'en',
    provider: 'bing',
  })
  await new Promise((resolve) => setTimeout(resolve, 10))
  await Promise.race([
    disposeCancelCtx.fiber.dispose(),
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error('dispose stalled on hanging cancel')), 1000),
    ),
  ])
  await assert.rejects(failingHttp, /disposed/)

  const { ctx: disposeCtx, translator: disposeTranslator } = await mountTranslator(
    t,
    () => new Promise(() => {}),
  )
  const inflight = disposeTranslator.translate({
    text: 'dispose while pending',
    sourceLanguage: 'auto',
    targetLanguage: 'en',
    provider: 'bing',
  })
  await new Promise((resolve) => setTimeout(resolve, 10))
  await disposeCtx.fiber.dispose()
  await assert.rejects(inflight, /disposed/)

  let resolveLateFetch
  let lateBodyCancelled = 0
  const { translator: lateTranslator } = await mountTranslatorWithFetch(
    t,
    () =>
      new Promise((resolve) => {
        resolveLateFetch = resolve
      }),
  )
  const lateAbort = new AbortController()
  const lateRun = lateTranslator.translate(
    { text: 'late response', sourceLanguage: 'auto', targetLanguage: 'en', provider: 'bing' },
    lateAbort.signal,
  )
  await new Promise((resolve) => setTimeout(resolve, 10))
  lateAbort.abort(new Error('abort before fetch settles'))
  await assert.rejects(lateRun, /abort before fetch settles/)
  resolveLateFetch({
    ok: true,
    status: 200,
    redirected: false,
    body: {
      cancel() {
        lateBodyCancelled += 1
        return Promise.resolve()
      },
    },
  })
  await new Promise((resolve) => setTimeout(resolve, 10))
  assert.equal(lateBodyCancelled, 1)
})
