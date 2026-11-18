import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { projectDotField } from '../dist/core/dot-field.js';
import { ACTIVITY_LABELS } from '../dist/core/activity-labels.js';

const KINDS = ['reading', 'editing', 'searching', 'testing', 'executing', 'browsing', 'computer', 'memory',
  'tool', 'thinking', 'responding', 'delegating'];
const activity = (verb, extra = {}) => ({ sessionId: 's', cursor: 1, observed: true, freshness: 'fresh',
  authoritativePresence: verb === 'waiting' ? 'needs_you' : verb === 'done' ? 'done' : 'working',
  activityKind: ['waiting', 'done'].includes(verb) ? null : verb, observedAtMs: 0, parallelAgentCount: 3,
  activeSpans: [], turnId: 't', turnOutcome: null, doneEffectId: verb === 'done' ? 't' : null, failedToolAge: null, ...extra });
const whale = (style) => projectDotField(activity('reading', { observed: false, freshness: 'missing' }), 0, true, 0, { bodyStyle: style });
const inside = (field) => field.points.length === 980 && field.materials.length === 980
  && field.points.every(p => p.every(v => Number.isFinite(v) && Math.abs(v) <= .94))
  && field.materials.every(m => m.length === 4 && m.every(Number.isFinite) && m[3] >= 0 && m[3] <= 1);

test('every activity forms its own moving shape from all 980 dots, deterministically, and returns to the whale', () => {
  for (const style of ['smooth', 'pixel']) {
    const home = whale(style);
    assert.equal(home.morph, 0);
    for (const verb of [...KINDS, 'waiting', 'done']) {
      const at = verb === 'done' ? [300, 600] : [3000, 4000];
      const formed = projectDotField(activity(verb), at[0], false, 0, { bodyStyle: style });
      assert.ok(inside(formed) && formed.morph === 1 && formed.bodyStyle === style, `${style} ${verb}`);
      assert.deepEqual(projectDotField(activity(verb), at[0], false, 0, { bodyStyle: style }), formed);
      assert.notDeepEqual(projectDotField(activity(verb), at[1], false, 0, { bodyStyle: style }).points, formed.points, `${verb} moves`);
      const still = projectDotField(activity(verb), at[0], true, 0, { bodyStyle: style });
      assert.ok(inside(still) && still.morph === 1);
      assert.deepEqual(projectDotField(activity(verb), at[1], true, 0, { bodyStyle: style }), still, `${verb} still is fixed`);
      assert.notDeepEqual(still.points, home.points);
      const settled = verb === 'done' ? 2600 : 5500;
      assert.equal(projectDotField(activity(verb), settled, false, 0, { bodyStyle: style }).morph, 0, `${verb} returns home`);
    }
  }
  assert.deepEqual(projectDotField(activity('tool'), 3000, true).points, projectDotField(activity('executing'), 3000, true).points);
  assert.notDeepEqual(projectDotField(activity('delegating', { parallelAgentCount: 2 }), 3000, true).points,
    projectDotField(activity('delegating'), 3000, true).points);
  for (const unobserved of [{ observed: false }, { freshness: 'stale' }, { authoritativePresence: 'idle' }])
    assert.equal(projectDotField(activity('reading', unobserved), 3000, false).morph, 0);
});

test('activity captions are the interface labels from the locale packs', () => {
  const ids = { reading: 'ToolFamilyRead', editing: 'ToolFamilyPatch', searching: 'ToolFamilyFind', testing: 'ToolFamilyVerify',
    executing: 'ToolFamilyRun', browsing: 'PetActivityBrowsing', computer: 'PetActivityComputer', memory: 'PetActivityMemory',
    tool: 'ToolFamilyGeneric', thinking: 'ToolFamilyThink', responding: 'PetModeReplyTitle', delegating: 'PhaseSubagents',
    waiting: 'PhaseWaitingOnYou', done: 'PhaseDone' };
  for (const [language, labels] of Object.entries(ACTIVITY_LABELS)) {
    const pack = JSON.parse(readFileSync(new URL(`../../crates/localization/locales/${language}.json`, import.meta.url), 'utf8'));
    assert.deepEqual(labels, Object.fromEntries(Object.entries(ids).map(([kind, id]) => [kind, pack[id]])), language);
  }
});
