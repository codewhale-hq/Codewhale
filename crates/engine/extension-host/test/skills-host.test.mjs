import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { activate, startHost } from './harness.mjs'

function plugin(t, body) {
  const directory = mkdtempSync(join(tmpdir(), 'cw-skill-root-'))
  const entry = join(directory, 'index.mjs')
  writeFileSync(entry, `export const inject = ['skills']\nexport function apply(ctx) {\n${body}\n}\n`)
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  return entry
}

test('real host admits detached skill root proposals and disposes only their owner', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const entry = plugin(t, `const root = { path: 'profiles/review-skills' }; ctx.skills.registerRoot(root); root.path = '../changed'`)
  const a = await activate(host, 'skill-a', entry), b = await activate(host, 'skill-b', entry)
  assert.equal(a.result.status, 'ok'); assert.equal(b.result.status, 'ok')
  const registrations = host.registry.filter(item => item.op === 'register' && item.kind === 'skill_root')
  assert.equal(registrations.length, 2)
  assert.deepEqual(registrations[0].spec, { name: 'profiles/review-skills', description: '' })
  assert.deepEqual(await host.call('ext/deactivate', { owner: a.ref }), { disposed: true, leaked: [] })
  assert.ok(host.registry.some(item => item.op === 'unregister' && item.handle === registrations[0].handle))
  assert.ok(!host.registry.some(item => item.op === 'unregister' && item.handle === registrations[1].handle))
})

test('invalid and duplicate skill roots fail activation and roll back proposals', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const cases = [
    [`ctx.skills.registerRoot({ path: '../outside' })`, /bundle-relative/],
    [`ctx.skills.registerRoot({ path: 'skills', watch: true })`, /only path/],
    [`ctx.skills.registerRoot({ path: 'skills' }); ctx.skills.registerRoot({ path: 'skills' })`, /already registered/],
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
  const { result } = await activate(host, 'refused-root', plugin(t, `ctx.skills.registerRoot({ path: 'skills' })`))
  assert.equal(result.status, 'failed')
  assert.match(result.diagnostic, /outside the reviewed inventory/)
})
