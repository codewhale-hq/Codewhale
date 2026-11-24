import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { PetNative } from '../dist/core/pet-native.js';
import { PetLiveTape, compilePetTelemetry } from '../dist/core/pet-telemetry.js';
import { petDemoEvents } from '../dist/core/pet-demo.js';

const points = JSON.stringify(readFileSync(new URL('../public/whale-points.tsv', import.meta.url), 'utf8').trim().split('\n').map(row => row.trim().split(/\s+/).map(Number)));
const human = compilePetTelemetry(petDemoEvents()).find(b => b.channel === 'human' && b.waiting);
const packet = sequence => ({ ...human, sequence, simTimeMs: sequence * 400 });
const line = sequence => JSON.stringify(packet(sequence)) + '\n';

test('live tails require advancement after attachment, replacement, invalid input and resume', () => {
  const live = new PetLiveTape();
  assert.equal(live.readTail(line(50)), undefined, 'Existing bytes establish a baseline');
  assert.equal(live.readTail(line(50)), undefined);
  assert.deepEqual(live.readTail(line(51)), packet(51));
  assert.equal(live.readTail(line(5)), undefined, 'Non-zero stale regression is ignored without lowering baseline');
  assert.equal(live.readTail(line(6)), undefined, 'Subsequent stale packet below high-water mark stays ignored');
  assert.deepEqual(live.readTail(line(52)), packet(52), 'Advancement past high-water mark still succeeds');
  assert.equal(live.readTail(line(0)), undefined, 'An in-place producer restart is a new baseline');
  assert.deepEqual(live.readTail(line(1)), packet(1));
  assert.equal(live.readTail(line(2).trimEnd()), undefined, 'Wait for a complete append');
  assert.deepEqual(live.readTail(line(2)), packet(2));
  assert.throws(() => live.readTail('{bad}\n'));
  assert.equal(live.readTail(line(3)), undefined);
  assert.deepEqual(live.readTail(line(4)), packet(4));
  live.readTail(''); assert.equal(live.readTail(line(5)), undefined);
  assert.deepEqual(live.readTail('partial first line\n' + line(6)), packet(6));
  live.reset(); assert.equal(live.readTail(line(7)), undefined);
  assert.deepEqual(live.readTail(line(8)), packet(8));
  assert.throws(() => live.readTail('x'.repeat(262_145)));
  assert.equal(live.readTail(line(9)), undefined);
});

test('native normal-tick live resume uses the world clock and discards old voices', () => {
  const pet = new PetNative(points, '', '[]', true);
  for (let i = 0; i < 90; i++) pet.step(1 / 30, true);
  pet.accept(JSON.stringify(packet(40)));
  for (let i = 0; i < 12; i++) pet.step(1 / 30, true);
  const before = JSON.parse(pet.snapshot());
  assert.equal(before.state.channel, 'human');
  assert.ok(before.state.observed >= .92);
  const resumedAt = pet.resumeEngine(), after = JSON.parse(pet.snapshot());
  assert.ok(resumedAt >= before.timeMs && resumedAt - before.timeMs <= 800, 'Skip only the unexpired live interval');
  assert.equal(after.state.observed, 0);
  assert.equal(after.needs, 'none'); assert.deepEqual(after.voices, []);
  const copy = new PetNative(points); copy.restoreRecording(pet.recording(true));
  assert.deepEqual(JSON.parse(copy.snapshot()), after);
  for (let i = 0; i < 60; i++) assert.equal(pet.step(1 / 30, true), copy.step(1 / 30, true));
});

test('native live delivery journals only newly observed packets across resume', () => {
  const pet = new PetNative(points, '', '[]', true);
  assert.equal(pet.acceptLiveTail(line(20)), false);
  assert.equal(JSON.parse(pet.recording()).tape.length, 1);
  assert.equal(pet.acceptLiveTail(line(21)), true);
  for (let i = 0; i < 12; i++) pet.step(1 / 30, true);
  assert.equal(JSON.parse(pet.snapshot()).state.channel, 'human');
  pet.resetLiveInput();
  assert.equal(pet.acceptLiveTail(line(22)), false, 'A background append establishes the resumed baseline');
  assert.equal(JSON.parse(pet.snapshot()).needs, 'none');
  assert.equal(pet.acceptLiveTail(line(23)), true);
  for (let i = 0; i < 12; i++) pet.step(1 / 30, true);
  assert.equal(JSON.parse(pet.snapshot()).state.channel, 'human');
});

test('a rejected single Engine observation cannot refresh or prune the owner reducer', () => {
  const pet = new PetNative(points, '', '[]', true);
  pet.observeEngine(JSON.stringify({ event: 'turn_started', turn_id: 't' }), 0);
  pet.observeEngine(JSON.stringify({ event: 'operation_activity_started', span_id: 's', activity_kind: 'editing' }), 10);
  const before = JSON.parse(pet.presentation()).activity;
  // Passes field validation, then fails the type-specific turn_id requirement.
  assert.throws(() => pet.observeEngine(JSON.stringify({ event: 'turn_started' }), 60_000), /Missing Engine turn_id/);
  const after = JSON.parse(pet.presentation()).activity;
  assert.equal(after.observedAtMs, 10, 'The rejected packet is not a fresh observation');
  assert.deepEqual(after, before);
  pet.observeEngine(JSON.stringify({ event: 'tool_call_heartbeat' }), 20);
  assert.equal(JSON.parse(pet.presentation()).activity.observedAtMs, 20, 'The clock was not advanced by the rejected packet');
});
