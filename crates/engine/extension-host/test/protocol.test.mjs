// Protocol conformance: the shared corpus that the Rust side also parses.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { FIXTURES } from './harness.mjs'
import { FrameDecoder, encodeFrame, validateMessage, MAGIC, MAX_FRAME } from '../dist/protocol.mjs'

const corpusDir = join(FIXTURES, 'protocol')
const corpus = readdirSync(corpusDir)
  .filter((name) => name.endsWith('.json'))
  .sort()
  .map((name) => ({ name, ...JSON.parse(readFileSync(join(corpusDir, name), 'utf8')) }))

test('the corpus is non-trivial in both directions', () => {
  for (const direction of ['host_to_core', 'core_to_host']) {
    assert.ok(corpus.some((c) => c.direction === direction && c.valid), `${direction} valid cases`)
    assert.ok(corpus.some((c) => c.direction === direction && !c.valid), `${direction} invalid cases`)
  }
})

// These shared corpus rows apply to both tiers; the generated method table
// governs builtin-only production requests.
for (const entry of corpus) {
  for (const tier of ['plugin', 'builtin']) {
    test(`corpus ${entry.name} (${tier} tier): ${entry.valid ? 'parses and round-trips' : 'is rejected'}`, () => {
      if (!entry.valid) {
        assert.throws(() => validateMessage(entry.frame, entry.direction, tier))
        return
      }
      validateMessage(entry.frame, entry.direction, tier)
      const [decoded] = new FrameDecoder().push(encodeFrame(entry.frame))
      assert.deepEqual(decoded, entry.frame)
      validateMessage(decoded, entry.direction, tier)
    })
  }
}

// The tier rule, against a table with a method reserved for the built-in tier
// independently of the generated production table.
const RESERVED = [
  { name: 'test/reserved', direction: 'host_to_core', request: true, params: 'EmptyParams', tiers: ['builtin'] },
  { name: 'test/reserved-in', direction: 'core_to_host', request: false, params: 'EmptyParams', tiers: ['builtin'] },
  { name: 'test/shared', direction: 'host_to_core', request: true, params: 'EmptyParams', tiers: ['plugin', 'builtin'] },
]

test('a method reserved for the builtin tier is refused to a plugin-tier host in both directions', () => {
  const asHost = { jsonrpc: '2.0', id: 1, method: 'test/reserved', params: {} }
  const toHost = { jsonrpc: '2.0', method: 'test/reserved-in', params: {} }
  assert.equal(validateMessage(asHost, 'host_to_core', 'builtin', RESERVED).method, 'test/reserved')
  assert.equal(validateMessage(toHost, 'core_to_host', 'builtin', RESERVED).method, 'test/reserved-in')
  assert.throws(() => validateMessage(asHost, 'host_to_core', 'plugin', RESERVED), /not allowed on the plugin tier/)
  assert.throws(() => validateMessage(toHost, 'core_to_host', 'plugin', RESERVED), /not allowed on the plugin tier/)
  // A method open to both tiers is open to both, and a method the table lacks is unknown, not "reserved".
  for (const tier of ['plugin', 'builtin']) {
    validateMessage({ jsonrpc: '2.0', id: 2, method: 'test/shared', params: {} }, 'host_to_core', tier, RESERVED)
    assert.throws(() => validateMessage({ jsonrpc: '2.0', id: 3, method: 'test/none', params: {} }, 'host_to_core', tier, RESERVED), /unknown/)
  }
  // The reservation names a direction: the same name the other way is not the reserved row.
  assert.throws(() => validateMessage({ jsonrpc: '2.0', id: 4, method: 'test/reserved', params: {} }, 'core_to_host', 'builtin', RESERVED), /unknown/)
})


test('frames split across chunks decode once, in order', () => {
  const bytes = Buffer.concat([encodeFrame({ a: 1 }), encodeFrame({ b: 'ü' })])
  const decoder = new FrameDecoder()
  const out = []
  for (let i = 0; i < bytes.length; i += 3) out.push(...decoder.push(bytes.subarray(i, i + 3)))
  assert.deepEqual(out, [{ a: 1 }, { b: 'ü' }])
})

test('bad magic, oversized length, and non-JSON payloads are framing errors', () => {
  assert.throws(() => new FrameDecoder().push(Buffer.from('NOPE\x00\x00\x00\x00')), /magic/)
  const huge = Buffer.alloc(8)
  MAGIC.copy(huge)
  huge.writeUInt32LE(MAX_FRAME + 1, 4)
  assert.throws(() => new FrameDecoder().push(huge), /MAX_FRAME/)
  const bad = Buffer.alloc(9)
  MAGIC.copy(bad)
  bad.writeUInt32LE(1, 4)
  bad.write('{', 8)
  assert.throws(() => new FrameDecoder().push(bad), /not JSON/)
})

test('wire JSON guard refuses values that would serialize differently', async () => {
  const { isJson } = await import('../src/json.ts')
  assert.equal(isJson({ a: [1, 'two', null, { b: true }] }), true)
  // A hole serializes as null, a non-index array property is dropped, a symbol
  // key is ignored, and a getter can answer differently when serialized.
  assert.equal(isJson(new Array(2)), false)
  const extra = [1]
  extra.note = 'dropped'
  assert.equal(isJson(extra), false)
  assert.equal(isJson({ [Symbol('s')]: 1 }), false)
  let reads = 0
  assert.equal(isJson({ get value() { reads += 1; return reads } }), false)
  assert.equal(isJson(Object.defineProperty({}, 'hidden', { value: 1, enumerable: false })), false)
  assert.equal(isJson(new Date(0)), false)
})
