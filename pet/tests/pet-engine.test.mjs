import test from 'node:test';
import assert from 'node:assert/strict';
import { PetEngineTelemetry } from '../dist/core/pet-engine.js';

test('typed operation activity carries a safe action identity and goes stale', () => {
  const e = new PetEngineTelemetry();
  for (const [i, kind] of ['reading','searching','editing','executing','testing','browsing','computer'].entries()) {
    const at = i * 20_000;
    e.observe({event:'turn_started',turn_id:'turn-'+i},at);
    e.observe({event:'operation_activity_started',span_id:'private-span-'+i,activity_kind:kind,action_id:'action-'+i},at);
    const activity=e.activity(at+100);
    assert.equal(activity.actionId,'action-'+i); assert.equal(activity.activeSpans[0].actionId,'action-'+i);
    assert.equal(activity.activityKind,kind); assert.equal(activity.authoritativePresence,'working');
    assert.equal(activity.freshness,'fresh'); assert.equal(activity.turnId,'turn-'+i);
    assert.deepEqual(Object.keys(activity).sort(),['actionId','activeSpans','activityKind','authoritativePresence','cursor','doneEffectId',
      'failedToolAge','freshness','observed','observedAtMs','parallelAgentCount','schemaVersion','sessionId','turnId','turnOutcome']);
    assert.equal(JSON.stringify(activity).includes('private-span'),false);
    const stale=e.activity(at+12_001);
    assert.equal(stale.freshness,'stale'); assert.equal(stale.activityKind,null); assert.equal(stale.actionId,undefined); assert.deepEqual(stale.activeSpans,[]);
  }
  // The pre-contract events carried tool names; the boundary refuses them.
  assert.throws(() => e.observe({event:'tool_call_started',tool_call_id:'a',tool_name:'read_file'},200_000),/Invalid Engine pet metadata fields/);
  assert.throws(() => e.observe({event:'turn_started'},200_000),/Missing Engine turn_id/);
  assert.throws(() => e.observe({event:'operation_activity_started',span_id:'a',activity_kind:'shell'},200_000));
  for (const action_id of ['', 'x'.repeat(257), '鲸'.repeat(86), 'unsafe\nname'])
    assert.throws(() => e.observe({event:'operation_activity_started',span_id:'a',activity_kind:'tool',action_id},200_000));
});
test('only a completed turn is Done; interrupted and failed turns are idle', () => {
  for (const [outcome, presence] of [['completed','done'],['interrupted','idle'],['failed','idle']]) {
    const e = new PetEngineTelemetry();
    e.observe({event:'turn_started',turn_id:'t'},0);
    e.observe({event:'operation_activity_started',span_id:'s',activity_kind:'editing'},10);
    e.observe({event:'turn_complete',turn_id:'t',turn_outcome:outcome},20);
    const activity=e.activity(30);
    assert.equal(activity.authoritativePresence,presence,outcome);
    assert.equal(activity.doneEffectId,outcome==='completed'?'t':null);
    assert.equal(activity.activityKind,null); assert.deepEqual(activity.activeSpans,[]);
  }
  const untracked = new PetEngineTelemetry();
  untracked.observe({event:'turn_complete',turn_outcome:'completed'},0);
  assert.equal(untracked.activity(10).authoritativePresence,'idle');
  assert.throws(() => untracked.observe({event:'turn_complete',turn_id:null,turn_outcome:'completed'},20));
  // `/purge`, an edit rejection or a mid-turn session switch complete with no
  // turn id. An outcome with no turn would fail the Rust projection
  // invariant (`turn_outcome` requires `turn_id`) and drop the shared frame.
  for (const outcome of ['completed','interrupted','failed']) {
    const e = new PetEngineTelemetry();
    e.observe({event:'turn_started',turn_id:'t'},0);
    e.observe({event:'turn_complete',turn_outcome:outcome},20);
    const activity=e.activity(30);
    assert.equal(activity.turnId,null,outcome); assert.equal(activity.turnOutcome,null,outcome);
    assert.equal(activity.authoritativePresence,'idle',outcome); assert.equal(activity.doneEffectId,null,outcome);
  }
});
test('a new turn drops spans left open by the previous turn', () => {
  const e = new PetEngineTelemetry();
  e.observe({event:'turn_started',turn_id:'t1'},0);
  e.observe({event:'operation_activity_started',span_id:'orphan',activity_kind:'editing'},10);
  e.observe({event:'turn_started',turn_id:'t2'},20);
  const activity=e.activity(30);
  assert.equal(activity.turnId,'t2'); assert.equal(activity.activityKind,null); assert.deepEqual(activity.activeSpans,[]);
});
test('waiting and concurrent activity stay bounded', () => {
  const e = new PetEngineTelemetry();
  for (const event of [{event:'turn_started',turn_id:'t'},{event:'operation_activity_started',span_id:'a',activity_kind:'computer'},
    ...Array.from({length:3},(_,i)=>({event:'agent_spawned',id:'private-'+i}))]) e.observe(event,0);
  e.confirmWaiting(200,false);
  const activity=e.activity(200);
  assert.equal(activity.activityKind,'computer');assert.equal(activity.parallelAgentCount,3);
  assert.equal(JSON.stringify(activity).includes('private-'),false);
  e.observe({event:'approval_required',id:'private'},200);
  e.confirmWaiting(600,true);
  const waiting=e.activity(600);
  assert.equal(waiting.authoritativePresence,'needs_you');assert.equal(waiting.activityKind,null);
  e.observe({event:'approval_resolved',id:'private',outcome:'denied'},700);
  e.confirmWaiting(800,false);
  assert.equal(e.activity(800).authoritativePresence,'working');
  assert.equal(new PetEngineTelemetry().activity(800).freshness,'missing');
});
test('Engine pulses expire; a late failed completion tears at receipt time without rewriting history', () => {
  const engine = new PetEngineTelemetry();
  engine.observe({ event: 'operation_activity_started', span_id: 'a', activity_kind: 'executing', action_id: 'exec_shell' }, 0);
  engine.observe({ event: 'tool_call_heartbeat' }, 300);
  const first = engine.bucket(0);
  assert.equal(first.channel, 'code'); assert.equal(first.activeMs[3], 300);
  assert.equal(engine.bucket(3).observed, 0);
  engine.observe({ event: 'operation_activity_completed', span_id: 'a', activity_kind: 'executing', outcome: 'failed' }, 5900);
  assert.equal(engine.activity(5900).failedToolAge.actionId, 'exec_shell');
  assert.equal(engine.activity(5900).actionId, 'exec_shell');
  assert.equal(engine.bucket(13).observed, 0);
  assert.equal(engine.bucket(14).channel, 'error'); assert.equal(engine.bucket(14).errors, 1);
  assert.deepEqual(engine.bucket(0), first);
  engine.observe({ event: 'thinking_started', index: 1 }, 5901);
  assert.equal(engine.activity(5901).actionId, undefined);

});
test('Engine worker boundary rejects payloads, invalid clocks and oversized active sets', () => {
  const e = new PetEngineTelemetry();
  assert.throws(() => e.observe({ event: 'response_delta', index: 0, delta: 'private' }, 0));
  assert.throws(() => e.observe({ event: 'response_delta', index: 0, channel: 'fabricated' }, 0));
  e.observe({ event: 'thinking_started', index: 0 }, 100);
  assert.throws(() => e.observe({ event: 'thinking_complete', index: 0 }, 90));
  for (let i = 1; i < 256; i++) e.observe({ event: 'thinking_started', index: i }, 100);
  assert.throws(() => e.observe({ event: 'thinking_started', index: 257 }, 100));
});

test('successive shared batches preserve active and waiting coverage exactly', () => {
  let shared = new PetEngineTelemetry();
  const direct = new PetEngineTelemetry();
  for (const [at, event, waiting] of [
    [0, {event:'operation_activity_started', span_id:'build', activity_kind:'executing'}, false],
    [300, {event:'tool_call_heartbeat'}, false],
    [600, {event:'tool_call_heartbeat'}, false],
    [800, {event:'operation_activity_completed', span_id:'build', activity_kind:'executing', outcome:'succeeded'}, false],
    [900, {event:'approval_required', id:'permission'}, true],
    [1200, {event:'agent_spawned', id:'worker'}, true],
    [1500, {event:'agent_progress', id:'worker', worker_status:'running'}, true],
  ]) {
    shared = shared.clone();
    for (const engine of [shared, direct]) {
      engine.observe(event, at);
      engine.confirmWaiting(at, waiting);
    }
    for (let bucket=0; bucket<=Math.floor(at/400); bucket++)
      assert.deepEqual(shared.bucket(bucket), direct.bucket(bucket));
  }
});
