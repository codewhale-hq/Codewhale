import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { activate, startHost } from './harness.mjs'

function plugin(t, body) {
  const directory = mkdtempSync(join(tmpdir(), 'cw-avatar-pack-'))
  const entry = join(directory, 'index.mjs')
  writeFileSync(entry, `export const inject = ['avatars']\nexport function apply(ctx) {\n${body}\n}\n`)
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  return entry
}

test('real host admits detached avatar pack proposals and disposes only their owner', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const entry = plugin(t, `const root = { path: 'avatars/avatar.json' }; ctx.avatars.registerPack(root); root.path = '../changed'`)
  const a = await activate(host, 'avatar-a', entry), b = await activate(host, 'avatar-b', entry)
  assert.equal(a.result.status, 'ok'); assert.equal(b.result.status, 'ok')
  const registrations = host.registry.filter(item => item.op === 'register' && item.kind === 'avatar_pack')
  assert.equal(registrations.length, 2)
  assert.deepEqual(registrations[0].spec, { name: 'avatars/avatar.json', description: '' })
  assert.deepEqual(await host.call('ext/deactivate', { owner: a.ref }), { disposed: true, leaked: [] })
  assert.ok(host.registry.some(item => item.op === 'unregister' && item.handle === registrations[0].handle))
  assert.ok(!host.registry.some(item => item.op === 'unregister' && item.handle === registrations[1].handle))
})

test('invalid and duplicate avatar packs fail activation and roll back proposals', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const cases = [
    [`ctx.avatars.registerPack({ path: '../outside' })`, /bundle-relative/],
    [`ctx.avatars.registerPack({ path: 'avatars/avatar.json', watch: true })`, /only path/],
    [`ctx.avatars.registerPack({ path: 'avatars/avatar.json' }); ctx.avatars.registerPack({ path: 'avatars/avatar.json' })`, /already registered/],
  ]
  for (const [index, [body, reason]] of cases.entries()) {
    const before = host.registry.length
    const { result } = await activate(host, `bad-root-${index}`, plugin(t, body))
    assert.equal(result.status, 'failed'); assert.match(result.diagnostic, reason)
    for (const item of host.registry.slice(before).filter(item => item.op === 'register')) {
      if (!host.registry.some(later => later.op === 'unregister' && later.handle === item.handle)) {
        await host.waitFor(message => message.method === 'registry/unregister' && message.params.handle === item.handle)
      }
    }
  }
})

test('real host preserves Rust root admission refusal in activation result', async (t) => {
  const host = await startHost({ admit: () => ({ refused: 'root is outside the reviewed inventory' }) })
  t.after(() => host.stop())
  const { result } = await activate(host, 'refused-root', plugin(t, `ctx.avatars.registerPack({ path: 'avatars/avatar.json' })`))
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /outside the reviewed inventory/)
})
