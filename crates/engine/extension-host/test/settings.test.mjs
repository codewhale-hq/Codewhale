import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { build } from 'esbuild'

const here = dirname(fileURLToPath(import.meta.url))
let settingsAdapter
let SettingsConflictError

before(async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-settings-'))
  const outfile = join(dir, 'settings.mjs')
  await build({ entryPoints: [join(here, '../src/dsh/settings.ts')], outfile, bundle: true, platform: 'node', format: 'esm', packages: 'external', absWorkingDir: join(here, '..'), logLevel: 'silent' })
  ;({ settingsAdapter, SettingsConflictError } = await import(pathToFileURL(outfile).href))
  process.on('exit', () => rmSync(dir, { recursive: true, force: true }))
})

function service(answers, pluginName = 'fixture.plugin') {
  const calls = []
  const host = {
    tier: 'plugin',
    ownerOf: () => ({ pluginName }),
    broker: async (ctx, name, method, input) => {
      calls.push({ name, method, input })
      const answer = answers[method]
      if (answer instanceof Error) throw answer
      return answer
    },
  }
  const instance = Object.create(settingsAdapter.define(host).prototype)
  instance.ctx = {}
  return { settings: instance, calls }
}

const row = { ns: 'fixture.plugin', writable: true, roots: ['model'], value: { model: 'small' }, revision: 'r1', applied: true, applies: 'reload' }

test('settings read and edit go to the settings broker as typed methods', async () => {
  const { settings, calls } = service({
    describe: row,
    update: { written: true, conflict: false, revision: 'r2', applies: 'reload' },
    replace: { written: true, conflict: false, revision: 'r3', applies: 'reload' },
    mutate: { written: false, conflict: false, revision: 'r3', applies: 'reload' },
  })
  assert.deepEqual(await settings.describe(), [row])
  assert.deepEqual(await settings.update('fixture.plugin', { model: 'large' }, 'r1'), { written: true, revision: 'r2', applies: 'reload' })
  await settings.replace('fixture.plugin', { model: 'tiny' })
  assert.equal((await settings.mutate('fixture.plugin', [{ op: 'unset', path: ['model'] }])).written, false)
  assert.deepEqual(calls, [
    { name: 'settings', method: 'describe', input: {} },
    { name: 'settings', method: 'update', input: { patch: { model: 'large' }, expected_revision: 'r1' } },
    { name: 'settings', method: 'replace', input: { value: { model: 'tiny' } } },
    { name: 'settings', method: 'mutate', input: { ops: [{ op: 'unset', path: ['model'] }] } },
  ])
})

test('another namespace, a malformed edit and a malformed answer never become a broker write', async () => {
  const { settings, calls } = service({ describe: { ns: 'fixture.plugin' }, update: { written: 'yes' } })
  await assert.rejects(settings.update('other.plugin', { model: 'x' }), /only its own settings namespace/)
  await assert.rejects(settings.update('', { model: 'x' }), /non-empty string/)
  await assert.rejects(settings.update('fixture.plugin', ['model']), /plain object/)
  await assert.rejects(settings.replace('fixture.plugin', null), /plain object/)
  await assert.rejects(settings.mutate('fixture.plugin', { op: 'set' }), /array of path operations/)
  await assert.rejects(settings.update('fixture.plugin', { model: 'x' }, 7), /expected revision/)
  assert.deepEqual(calls, [])
  await assert.rejects(settings.describe(), /invalid settings broker response/)
  await assert.rejects(settings.update('fixture.plugin', { model: 'x' }), /invalid settings broker response/)
})

test('a stale edit is a conflict and a broker refusal reaches the plugin as the refusal', async () => {
  const stale = service({ update: { written: false, conflict: true, revision: 'r9', applies: 'reload' } })
  await assert.rejects(stale.settings.update('fixture.plugin', { model: 'x' }, 'r1'), (error) => {
    assert.ok(error instanceof SettingsConflictError)
    assert.equal(error.code, 'SETTINGS_CONFLICT')
    assert.equal(error.expected, 'r1')
    assert.equal(error.actual, 'r9')
    return true
  })
  const refused = service({ update: new Error('that setting is outside this plugin\'s reviewed settings'), describe: new Error('the settings broker is not available yet') })
  await assert.rejects(refused.settings.update('fixture.plugin', { internal: 1 }), /outside this plugin's reviewed settings/)
  await assert.rejects(refused.settings.describe(), /not available yet/)
})
