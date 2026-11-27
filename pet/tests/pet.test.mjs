import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { PetSim, REST_STATE, digest, CHANNELS } from '../dist/core/pet-sim.js';
import { compilePetTelemetry, encodePetJSONL, decodePetJSONL } from '../dist/core/pet-telemetry.js';

const points = readFileSync(new URL('../public/whale-points.tsv', import.meta.url), 'utf8').trim().split('\n').map(row => row.trim().split(/\s+/).map(Number));
const event = (overrides = {}) => ({ schemaVersion: 1, id: 'e', traceId: 't', startTime: 0, endTime: 400,
  agentId: 'parent', category: 'tool', name: 'work', status: 'success', attributes: {}, ...overrides });

test('span time beats onset count; long tools remain measured; containers are excluded', () => {
  const e = [event({ id: 'root', category: 'orchestration', endTime: 4000 }),
    event({ id: 'long', parentId: 'root', category: 'reasoning', endTime: 4000 }),
    ...Array.from({ length: 8 }, (_, i) => event({ id: String(i), startTime: i * 10, endTime: i * 10 }))];
  const b = compilePetTelemetry(e);
  assert.equal(b[0].channel, 'reasoning'); assert.equal(b[9].channel, 'reasoning');
  assert.equal(b[0].activeMs[9], 0); assert.equal(b[0].activeMs[0], 400);
});
test('half-open bins retain endpoint onsets and distinguish unknown duration from idle', () => {
  const b = compilePetTelemetry([event({ endTime: 800 }), event({ id: 'last', startTime: 800, endTime: 800, category: 'human' })], 1600);
  assert.equal(b[2].channel, 'human'); assert.equal(b[2].onsets[11], 1); assert.equal(b[3].observed, 0); assert.equal(b[3].channel, 'other');
  const open = compilePetTelemetry([event({ openEnded: true, endTime: 3000 })], 1200);
  assert.equal(open[0].onsets[1], 1); assert.equal(open[0].activeMs[1], 0); assert.equal(open[1].observed, 0);
});
test('repeat density is causal; independent future events do not rewrite past buckets', () => {
  const first = [event({ endTime: 80 })], future = Array.from({ length: 7 }, (_, i) => event({ id: `f${i}`, startTime: 400 + i * 400, endTime: 480 + i * 400 }));
  const a = compilePetTelemetry(first), b = compilePetTelemetry([...first, ...future]);
  assert.deepEqual(a[0], b[0]); assert.ok(b[4].coherence < b[0].coherence);
});
test('errors, human spans and concurrent peers select semantic gaits', () => {
  assert.equal(compilePetTelemetry([event({ category: 'reasoning', status: 'error' })])[0].channel, 'error');
  const wait = compilePetTelemetry([event({ category: 'human', status: 'pending' })])[0];
  assert.equal(wait.channel, 'human'); assert.equal(wait.waiting, true);
  const peers = compilePetTelemetry(['a', 'b', 'c'].map(id => event({ id, agentId: id })))[0];
  assert.equal(peers.channel, 'agent'); assert.deepEqual(peers.agentIds, ['a', 'b', 'c']);
});
test('updates deduplicate, multiple traces reject, wire rejects corrupt values', () => {
  assert.equal(compilePetTelemetry([event(), event({ endTime: 800 })])[0].onsets[1], 1);
  assert.throws(() => compilePetTelemetry([event(), event({ traceId: 'another' })]), /one trace/);
  const b = compilePetTelemetry([event()]); assert.deepEqual(decodePetJSONL(encodePetJSONL(b)), b);
  for (const edit of [{ activity: null }, { observed: 2 }, { sequence: 8 }, { channel: 'fake' }, { onsets: [1] }])
    assert.throws(() => decodePetJSONL(JSON.stringify({ ...b[0], ...edit })));
  assert.throws(() => compilePetTelemetry([event({ endTime: Infinity })]));
});
test('reduced motion has no settling, jitter, colour fade or tear decay under constant input', () => {
  for (const ch of CHANNELS) {
    const sim = new PetSim(points), state = { ...REST_STATE, channel: ch.key, activity: .8, coherence: .4 };
    sim.step(1 / 30, state, { motion: false, sensitivity: 1 }); const first = digest(sim);
    for (let i = 0; i < 90; i++) sim.step(1 / 30, state, { motion: false, sensitivity: 1 });
    assert.equal(digest(sim), first, ch.key);
  }
});
