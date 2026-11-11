import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'

const here = dirname(fileURLToPath(import.meta.url))
let catalog

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-catalog-'))
  const outfile = join(dir, 'catalog.mjs')
  await build({ entryPoints: [join(here, '../src/dsh/catalog.ts')], outfile, bundle: true, platform: 'node', format: 'esm', packages: 'external', absWorkingDir: join(here, '..'), logLevel: 'silent' })
  catalog = await import(pathToFileURL(outfile).href)
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
        const answer = typeof answers[method] === 'function' ? answers[method](input) : answers[method]
        if (answer instanceof Error) throw answer
        return answer
      },
    },
  }
}

function serviceOf(adapter, host) {
  const service = Object.create(adapter.define(host).prototype)
  service.ctx = {}
  return service
}

const brokerRefusal = (code, refusal, message) => Object.assign(new Error(message), { code, data: { dsh_refusal: refusal, service: 'catalog' } })

test('a balance widget reads an absent account, an unknown balance and a known balance, and never a key', async () => {
  const answers = { balance: { status: 'absent' } }
  const { host, calls } = brokerHost(answers)
  const account = serviceOf(catalog.deepseekAccountAdapter, host)
  const client = { version: '0.4.2', locale: 'zh_CN', timezoneOffsetSeconds: 28800 }

  assert.equal(await account.getBalance(client), null)
  answers.balance = { status: 'unknown' }
  assert.deepEqual(await account.getBalance(client), { status: 'failed' })
  answers.balance = { status: 'known', currency: 'CNY', total: '123.45', topped_up: '100.00', granted: '23.45' }
  const ready = await account.getBalance(client)
  assert.deepEqual(ready, { status: 'ready', value: [{ currency: 'CNY', balance: '100.00' }], bonusWallets: [{ currency: 'CNY', balance: '23.45' }] })
  const shown = [...ready.value, ...ready.bonusWallets].reduce((sum, wallet) => sum + Number(wallet.balance), 0)
  assert.equal(shown.toFixed(2), '123.45')
  answers.balance = { status: 'known', currency: 'USD', total: '12.50', topped_up: '', granted: '' }
  assert.deepEqual(await account.getBalance(client), { status: 'ready', value: [{ currency: 'USD', balance: '12.50' }], bonusWallets: [] })

  assert.deepEqual(calls.map((call) => [call.service, call.method, call.input]), Array(4).fill(['catalog', 'balance', {}]))
  for (const method of ['resolveToken', 'getPlatformSession', 'getState', 'startSignIn', 'signOut', 'rejectToken']) {
    assert.throws(() => account[method]('https://api.deepseek.com'), (error) => error.code === -32001 && error.message.includes(`deepseekAccount.${method} is not provided`))
  }
  assert.equal(calls.length, 4)

  answers.balance = { status: 'known', currency: 'CNY', total: '1', topped_up: '1', granted: '0', api_key: 'sk-raw' }
  assert.deepEqual(Object.keys(await catalog.accountBalance(host, {})).sort(), ['currency', 'granted', 'status', 'topped_up', 'total'])
  answers.balance = { status: 'ready', value: [] }
  await assert.rejects(account.getBalance(client), /invalid catalog broker balance/)
  answers.balance = brokerRefusal(-32001, 'not_available', 'the catalog broker is not available yet')
  await assert.rejects(account.getBalance(client), /not available yet/)
})

test('a market plugin asking to install gets the reviewed-install answer, and nothing installs or removes', async () => {
  const review = {
    status: 'review_required', package: '@acme/review', version: '1.10.0', plugin: 'review', spec: 'npm:@acme/review@1.10.0',
    command: '/plugin install npm:@acme/review@1.10.0',
    message: 'Codewhale installs a plugin only through its reviewed install flow; nothing was installed. Review and install @acme/review@1.10.0 with `/plugin install npm:@acme/review@1.10.0`.',
  }
  const answers = { 'install.plan': review }
  const { host, calls } = brokerHost(answers)
  const manager = serviceOf(catalog.pluginManagerAdapter, host)

  const requested = await manager.installBundle('@acme/review@1.10.0', { requestId: 'r1', registry: 'https://registry.example.com' })
  assert.deepEqual(requested, {
    changed: false, application: 'failed', stage: 'install', target: '@acme/review@1.10.0',
    error: { code: 'management-required', diagnostic: review.message, message: review.message },
  })
  assert.deepEqual(calls[0], { service: 'catalog', method: 'install.plan', input: { package: '@acme/review', version: '1.10.0', registry: 'https://registry.example.com' } })
  await manager.installBundle('dsh-demo@1.2.3')
  assert.deepEqual(calls[1].input, { package: 'dsh-demo', version: '1.2.3' })

  answers['install.plan'] = { status: 'already_present', package: 'dsh-demo', version: '1.2.3', plugin: 'dsh-demo', command: '/plugin trust dsh-demo', message: 'A plugin named `dsh-demo` is already present; nothing was installed.' }
  assert.equal((await manager.installBundle('dsh-demo@1.2.3')).error.code, 'already-installed')

  answers['install.plan'] = brokerRefusal(-32002, 'refused', '`dsh-demo@^1.2.3` is not an exact npm package version Codewhale can fetch')
  const range = await manager.installBundle('dsh-demo@^1.2.3')
  assert.equal(range.error.code, 'invalid-spec')
  assert.match(range.error.diagnostic, /not an exact npm package version/)

  const sent = calls.length
  for (const spec of ['dsh-demo', '@acme/review', 'git+https://github.com/acme/review.git#0123456789abcdef0123456789abcdef01234567', 'dsh-demo@', 7]) {
    const refused = await manager.installBundle(spec)
    assert.equal(refused.error.code, 'invalid-spec', String(spec))
    assert.equal(refused.application, 'failed')
  }
  assert.equal((await manager.installBundle('dsh-demo@1.2.3', { registry: 7 })).error.code, 'invalid-spec')
  assert.deepEqual(await manager.removeBundle('dsh-demo'), {
    changed: false, application: 'failed', stage: 'remove', target: 'dsh-demo',
    error: { code: 'management-required', diagnostic: 'Codewhale removes a plugin only from its own plugin manager, after you disable it: /plugin uninstall', message: 'Codewhale removes a plugin only from its own plugin manager, after you disable it: /plugin uninstall' },
  })
  assert.deepEqual(await manager.cancelInstall('r1'), { status: 'not-running' })
  assert.equal(calls.length, sent, 'a spec that is not name@exact-version, a removal and a cancel never reach the broker')

  answers['install.plan'] = brokerRefusal(-32002, 'ticket_refused', 'the invocation this dsh/broker:catalog belongs to has ended')
  await assert.rejects(manager.installBundle('dsh-demo@1.2.3'), /has ended/)
  answers['install.plan'] = { status: 'installed', package: 'dsh-demo', version: '1.2.3', plugin: 'dsh-demo', command: 'x', message: 'x' }
  await assert.rejects(manager.installBundle('dsh-demo@1.2.3'), /invalid catalog broker install plan/)
})

test('a model-proxy directory is built from approved provider routes, and an unapproved route is the refusal', async () => {
  const gateway = { provider: 'gateway', base_url: 'https://gateway.example/api', default_model: 'tiny-model', models: ['tiny-model', 'large-model'] }
  const answers = {
    'routes.list': { routes: [gateway] },
    'routes.resolve': (input) => input.provider === 'gateway'
      ? { route: gateway, model: input.model ?? gateway.default_model }
      : brokerRefusal(-32002, 'refused', `\`${input.provider}\` is not a provider route approved for this plugin`),
  }
  const { host, calls } = brokerHost(answers)

  const routes = await catalog.approvedRoutes(host, {})
  const directory = {
    providers: routes.map((route) => ({ provider: route.provider, active: true })),
    models: routes.map((route) => ({ provider: route.provider, models: route.models.map((id) => ({ id })) })),
  }
  assert.deepEqual(directory, {
    providers: [{ provider: 'gateway', active: true }],
    models: [{ provider: 'gateway', models: [{ id: 'tiny-model' }, { id: 'large-model' }] }],
  })

  assert.deepEqual(await catalog.resolveRoute(host, {}, 'gateway', 'large-model'), { ...gateway, model: 'large-model' })
  assert.deepEqual(await catalog.resolveRoute(host, {}, 'gateway'), { ...gateway, model: 'tiny-model' })
  await assert.rejects(catalog.resolveRoute(host, {}, 'openai', 'gpt-x'), /not a provider route approved for this plugin/)
  assert.deepEqual(calls.map((call) => [call.service, call.method, call.input]), [
    ['catalog', 'routes.list', {}],
    ['catalog', 'routes.resolve', { provider: 'gateway', model: 'large-model' }],
    ['catalog', 'routes.resolve', { provider: 'gateway' }],
    ['catalog', 'routes.resolve', { provider: 'openai', model: 'gpt-x' }],
  ])

  await assert.rejects(catalog.resolveRoute(host, {}, '', 'x'), (error) => error.code === -32602)
  await assert.rejects(catalog.resolveRoute(host, {}, 'gateway', 7), (error) => error.code === -32602)
  assert.equal(calls.length, 4)
  answers['routes.list'] = { routes: [{ provider: 'gateway', base_url: 'https://gateway.example/api', default_model: null, models: [7] }] }
  await assert.rejects(catalog.approvedRoutes(host, {}), /invalid catalog broker route/)
  answers['routes.list'] = [gateway]
  await assert.rejects(catalog.approvedRoutes(host, {}), /invalid catalog broker routes/)
})
