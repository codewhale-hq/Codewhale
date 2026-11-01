import test from 'node:test'
import assert from 'node:assert/strict'
import { SkillRoots, normalizeSkillRoot } from '../dist/skills.mjs'

function fixture({ deferred = false, refused } = {}) {
  const calls = [], pending = []
  let next = 0
  const rpc = { request(method, params) {
    calls.push({ method, params })
    if (method === 'registry/unregister') return Promise.resolve({})
    const result = refused ? { refused } : { handle: ++next }
    if (!deferred) return Promise.resolve(result)
    return new Promise(resolve => pending.push(() => resolve(result)))
  } }
  const roots = new SkillRoots(rpc, owner => owner.roots, () => {})
  const owner = (id) => ({ ref: { plugin_id: id, generation: 1, owner_token: id }, state: 'active', pendingRegistrations: new Set(), refusals: [], roots: new Map() })
  return { roots, calls, pending, owner }
}
const settled = async owner => Promise.all([...owner.pendingRegistrations])

test('skill roots snapshot only bounded clean bundle-relative paths', () => {
  const input = { path: 'profiles/review-skills' }
  const normalized = normalizeSkillRoot(input)
  input.path = '../outside'
  assert.deepEqual(normalized, { path: 'profiles/review-skills' })
  assert.ok(Object.isFrozen(normalized))
  for (const path of ['', '/absolute', '../outside', './same', 'a/../b', 'a//b', 'a/', 'C:/disk', 'a\\b', 'a\u001bb', '界'.repeat(171)]) {
    assert.throws(() => normalizeSkillRoot({ path }), /bounded bundle-relative/)
  }
  for (const value of [null, [], 'path', { path: 'good', priority: 1 }]) assert.throws(() => normalizeSkillRoot(value))
})

test('owned skill roots dispose exact handles without crossing equal-path owners', async () => {
  const { roots, calls, owner } = fixture()
  const a = owner('a'), b = owner('b')
  const undo = roots.register(a, { path: 'profiles/skills' })
  roots.register(b, { path: 'profiles/skills' })
  await Promise.all([settled(a), settled(b)])
  assert.deepEqual(calls.filter(call => call.method === 'registry/register').map(call => call.params.spec), [
    { name: 'profiles/skills', description: '' }, { name: 'profiles/skills', description: '' },
  ])
  undo(); undo()
  await Promise.resolve()
  assert.deepEqual(calls.filter(call => call.method === 'registry/unregister').map(call => call.params.handle), [1])
  assert.equal(b.roots.size, 1)
  roots.forget(b)
  await Promise.resolve()
  assert.deepEqual(calls.filter(call => call.method === 'registry/unregister').map(call => call.params.handle), [1, 2])
})

test('pending roots count toward limits and disposal before admission cannot remove replacement', async () => {
  const { roots, calls, pending, owner } = fixture({ deferred: true })
  const a = owner('a')
  const old = roots.register(a, { path: 'skills' })
  assert.throws(() => roots.register(a, { path: 'skills' }), /already registered/)
  old()
  roots.register(a, { path: 'skills' })
  for (let i = 0; i < 7; i++) roots.register(a, { path: `s${i}` })
  assert.throws(() => roots.register(a, { path: 'ninth' }), /limit/)
  pending.forEach(resolve => resolve())
  await settled(a)
  assert.ok(calls.some(call => call.method === 'registry/unregister' && call.params.handle === 1))
  assert.ok(!calls.some(call => call.method === 'registry/unregister' && call.params.handle === 2))
  roots.forget(a)
  roots.register(a, { path: 'after-disposal' })
  pending.at(-1)()
  await settled(a)
  assert.equal(a.roots.size, 1)
})

test('host root cap includes multiple owners and recovers after owner teardown', async () => {
  const { roots, owner } = fixture()
  const owners = Array.from({ length: 8 }, (_, i) => owner(String(i)))
  for (const current of owners) for (let i = 0; i < 8; i++) roots.register(current, { path: `s${i}` })
  assert.throws(() => roots.register(owner('overflow'), { path: 's' }), /limit/)
  await Promise.all(owners.map(settled))
  roots.forget(owners[0])
  const replacement = owner('replacement')
  roots.register(replacement, { path: 's' })
  await settled(replacement)
  assert.equal(replacement.roots.size, 1)
})

test('revoked owner cannot register and a late admitted root is withdrawn', async () => {
  const { roots, calls, pending, owner } = fixture({ deferred: true })
  const a = owner('a')
  roots.register(a, { path: 'skills' })
  a.state = 'disposed'
  roots.forget(a)
  assert.throws(() => roots.register(a, { path: 'late' }), /not live/)
  pending.forEach(resolve => resolve())
  await settled(a)
  assert.equal(a.roots.size, 0)
  assert.ok(calls.some(call => call.method === 'registry/unregister' && call.params.handle === 1))
})

test('core refusal is preserved and local reservations retire with owner', async () => {
  const { roots, owner } = fixture({ refused: 'reviewed bundle was changed' })
  const a = owner('a')
  roots.register(a, { path: 'skills' })
  await settled(a)
  assert.match(a.refusals[0], /skill_root.*reviewed bundle was changed/)
  assert.equal(a.roots.size, 0)
  roots.forget(a)
  roots.register(a, { path: 'skills' })
  await settled(a)
})
