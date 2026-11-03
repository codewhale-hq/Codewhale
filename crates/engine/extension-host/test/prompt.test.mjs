// Author prompt contributions exercised through the committed host and CWX1.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { activate, startHost } from './harness.mjs'

function plugin(t, body) {
  const directory = mkdtempSync(join(tmpdir(), 'cw-prompt-section-'))
  const entry = join(directory, 'index.mjs')
  writeFileSync(entry, `export const inject = ['prompt']\nexport function apply(ctx) {\n${body}\n}\n`)
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  return entry
}

test('fake core drops late replies after host shutdown closes stdin', async () => {
  const host = await startHost()
  const stopping = host.stop()
  assert.equal(host.send({ jsonrpc: '2.0', method: '$/cancel', params: { id: 1 } }), false)
  await stopping
})

test('prompt sections detach text, allow owner-local ids and dispose exact registrations', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const entry = plugin(t, `
    const section = { id: 'rules', text: 'First line\\nSecond line' }
    ctx.prompt.registerSection(section)
    section.text = 'mutated after registration'
  `)
  const first = await activate(host, 'first-prompt', entry)
  const second = await activate(host, 'second-prompt', entry)
  assert.equal(first.result.status, 'ok')
  assert.equal(second.result.status, 'ok')
  const registrations = host.registry.filter((item) => item.op === 'register' && item.kind === 'prompt_section')
  assert.equal(registrations.length, 2)
  assert.deepEqual(registrations.map((item) => item.spec), [
    { name: 'rules', description: 'First line\nSecond line' },
    { name: 'rules', description: 'First line\nSecond line' },
  ])
  assert.deepEqual(await host.call('ext/deactivate', { owner: first.ref }), { disposed: true, leaked: [] })
  assert.ok(host.registry.some((item) => item.op === 'unregister' && item.handle === registrations[0].handle))
  assert.ok(!host.registry.some((item) => item.op === 'unregister' && item.handle === registrations[1].handle))
})

test('disposing before admission permits a new same-id section without removing its handle', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const entry = plugin(t, `
    const dispose = ctx.prompt.registerSection({ id: 'rules', text: 'old' })
    dispose()
    dispose()
    ctx.prompt.registerSection({ id: 'rules', text: 'new' })
  `)
  const { result, ref } = await activate(host, 'replacement-prompt', entry)
  assert.equal(result.status, 'ok')
  const registrations = host.registry.filter((item) => item.op === 'register' && item.kind === 'prompt_section')
  assert.equal(registrations.length, 2)
  assert.ok(host.registry.some((item) => item.op === 'unregister' && item.handle === registrations[0].handle))
  assert.ok(!host.registry.some((item) => item.op === 'unregister' && item.handle === registrations[1].handle))
  await host.call('ext/deactivate', { owner: ref })
  assert.ok(host.registry.some((item) => item.op === 'unregister' && item.handle === registrations[1].handle))
})

test('invalid, duplicate and oversized UTF-8 sections fail activation and roll back', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const cases = [
    [`ctx.prompt.registerSection({ id: 'rules', text: '  ' })`, /non-empty text/],
    [`ctx.prompt.registerSection({ id: 'rules', text: 'escape\\u001b[31m' })`, /control characters/],
    [`ctx.prompt.registerSection({ id: 'rules', text: 'x', order: -1 })`, /only id and text/],
    [`ctx.prompt.registerSection({ id: 'rules', text: '界'.repeat(1366) })`, /4096 UTF-8 bytes/],
    [`ctx.prompt.registerSection({ id: 'rules', text: 'first' }); ctx.prompt.registerSection({ id: 'rules', text: 'second' })`, /already registered/],
    [`for (let i = 0; i < 9; i++) ctx.prompt.registerSection({ id: 's' + i, text: 'x'.repeat(4096) })`, /byte limit/],
    [`for (let i = 0; i < 129; i++) ctx.prompt.registerSection({ id: 's' + i, text: 'x' })`, /registration limit/],
  ]
  for (const [index, [body, reason]] of cases.entries()) {
    const before = host.registry.length
    const { result } = await activate(host, `bad-prompt-${index}`, plugin(t, body))
    assert.equal(result.status, 'failed')
    assert.match(result.diagnostic, reason)
    const admitted = host.registry.slice(before).filter((item) => item.op === 'register' && item.kind === 'prompt_section')
    for (const registration of admitted) {
      if (!host.registry.some((item) => item.op === 'unregister' && item.handle === registration.handle)) {
        await host.waitFor((message) => message.method === 'registry/unregister' && message.params.handle === registration.handle, 2000)
      }
      assert.ok(host.registry.some((item) => item.op === 'unregister' && item.handle === registration.handle), `registration ${registration.handle} rolled back`)
    }
  }
})

test('host UTF-8 budget counts pending sections across owners and recovers after teardown', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const large = plugin(t, `for (let i = 0; i < 8; i++) ctx.prompt.registerSection({ id: 's' + i, text: 'x'.repeat(4096) })`)
  const active = []
  for (let index = 0; index < 4; index++) {
    const owner = await activate(host, `full-prompt-${index}`, large)
    assert.equal(owner.result.status, 'ok')
    active.push(owner.ref)
  }
  const small = plugin(t, `ctx.prompt.registerSection({ id: 'rules', text: 'one' })`)
  const refused = await activate(host, 'over-host-budget', small)
  assert.equal(refused.result.status, 'failed')
  assert.match(refused.result.diagnostic, /byte limit/)
  await host.call('ext/deactivate', { owner: active[0] })
  assert.equal((await activate(host, 'after-teardown', small)).result.status, 'ok')
})

test('core refusal fails prompt activation with its reason', async (t) => {
  const host = await startHost({ admit: () => ({ refused: 'reviewed Native authority is gone' }) })
  t.after(() => host.stop())
  const { result } = await activate(host, 'refused-prompt', plugin(t, `ctx.prompt.registerSection({ id: 'rules', text: 'text' })`))
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /reviewed Native authority is gone/)
})


test('Native source preserves exact non-ASCII prompt text on both runtimes', async t => {
  const host=await startHost();t.after(()=>host.stop())
  const text='中文 界 日本語 한국어 🌊 café'
  const entry=plugin(t,`ctx.prompt.registerSection({id:'unicode',text:${JSON.stringify(text)}})`)
  const mounted=await activate(host,'unicode-prompt-source',entry)
  assert.equal(mounted.result.status,'ok',mounted.result.diagnostic)
  assert.equal(host.registry.find(row=>row.op==='register' && row.kind==='prompt_section').spec.description,text)
  assert.deepEqual(await host.call('ext/deactivate',{owner:mounted.ref}),{disposed:true,leaked:[]})
})


test('template sections keep Core-owned values unresolved and share literal lifecycle and bounds', async t => {
  const host = await startHost(); t.after(() => host.stop())
  const source = plugin(t, `
    const definition = {id:'persona-prefix',text:'Model {{model}} at {{cwd}}. lone {{',interpolate:'model-cwd'}
    ctx.prompt.registerSection(definition)
    definition.text='forged values'
    ctx.prompt.registerSection({id:'literal',text:'{{model}} stays literal'})
  `)
  const mounted = await activate(host, 'templates', source)
  assert.equal(mounted.result.status, 'ok', mounted.result.diagnostic)
  const rows = host.registry.filter(row => row.op === 'register')
  assert.equal(rows.find(row => row.kind === 'prompt_template').spec.description, 'Model {{model}} at {{cwd}}. lone {{')
  assert.equal(rows.find(row => row.kind === 'prompt_section').spec.description, '{{model}} stays literal')
  assert.deepEqual(await host.call('ext/deactivate', {owner:mounted.ref}), {disposed:true,leaked:[]})
  for (const row of rows) assert.ok(host.registry.some(event => event.op === 'unregister' && event.handle === row.handle))
  for (const [index, text] of ['{{model_id}}','{{}}','{{ model }}','{{{cwd}}'].entries()) {
    const bad = await activate(host, 'bad-template-'+index, plugin(t, `ctx.prompt.registerSection({id:'invalid',text:${JSON.stringify(text)},interpolate:'model-cwd'})`))
    assert.equal(bad.result.status, 'failed'); assert.match(bad.result.diagnostic,/prompt variable/)
  }
  const duplicate = await activate(host, 'duplicate-template', plugin(t, `ctx.prompt.registerSection({id:'same',text:'{{model}}',interpolate:'model-cwd'});ctx.prompt.registerSection({id:'same',text:'literal'})`))
  assert.equal(duplicate.result.status, 'failed'); assert.match(duplicate.result.diagnostic,/already registered/)
})
