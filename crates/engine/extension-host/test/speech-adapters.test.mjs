import test from 'node:test'
import assert from 'node:assert/strict'
import { transformStockSnapshot } from '../dist/stock-adapters.mjs'
import { createHarnessModule } from '../dist/builtin/harness.mjs'
const project = (operation, input) => ({ kind: 'stock_adapter', operation, input })
const options = extra => project('speech_options', { surface: 'tool', model: null, instruction: null, voice_prompt: null, has_clone_path: false, has_voice: false, voice_nonempty: false, voice_is_data_uri: false, ...extra })
const metadata = input => transformStockSnapshot(input).result.metadata
const format = (value, surface = 'tool') => project('speech_format', { surface, format: value })

test('speech default, design and clone selection use captured presence only', () => {
  assert.deepEqual(metadata(options({})), { model: 'mimo-v2.5-tts', instruction: null, voice: 'default' })
  assert.deepEqual(metadata(options({ voice_prompt: 'Warm', instruction: ' Slow ' })), { model: 'mimo-v2.5-tts-voicedesign', instruction: 'Warm\n\nSlow', voice: 'omit' })
  assert.deepEqual(metadata(options({ has_clone_path: true })), { model: 'mimo-v2.5-tts-voiceclone', instruction: null, voice: 'clone' })
  assert.deepEqual(metadata(options({ has_voice: true, voice_nonempty: true, voice_is_data_uri: true })), { model: 'mimo-v2.5-tts-voiceclone', instruction: null, voice: 'raw' })
})
test('explicit canonical model wins, while actual voice remains in Core', () => {
  assert.equal(metadata(options({ model: 'mimo-v2-tts', voice_prompt: 'Design prompt' })).model, 'mimo-v2-tts')
  assert.equal(metadata(options({ model: 'mimo-v2.5-tts-voicedesign', has_voice: true, voice_nonempty: true, instruction: 'warm' })).voice, 'omit')
  assert.equal(metadata(options({ model: 'mimo-v2.5-tts-voicedesign', has_clone_path: true, instruction: 'warm' })).voice, 'clone')
  assert.deepEqual(Object.keys(metadata(options({ has_voice: true, voice_nonempty: true }))).sort(), ['instruction', 'model', 'voice'])
})
test('Rust whitespace including NEL is trimmed but BOM remains significant', () => {
  assert.equal(metadata(options({ instruction: '\u0085\u2007calm\u3000' })).instruction, 'calm')
  assert.equal(metadata(options({ instruction: '\ufeffcalm\ufeff' })).instruction, '\ufeffcalm\ufeff')
  assert.equal(metadata(options({ voice_prompt: '\u0085', instruction: '\u2007', model: 'mimo-v2-tts' })).instruction, null)
  assert.equal(metadata(options({ voice_prompt: '', instruction: ' warm ' })).model, 'mimo-v2.5-tts-voicedesign')
})
test('ASCII format normalization and pcm alias match the Rust contract', () => {
  for (const [value, expected] of [['WAV', 'wav'], [' pcm ', 'pcm16'], ['\u0085MP3\u0085', 'mp3'], ['pcm16', 'pcm16']]) assert.deepEqual(metadata(format(value)), { format: expected })
  assert.equal(transformStockSnapshot(format('\ufeffwav')).result.success, false)
  assert.equal(metadata(format('flac')).error, "unsupported speech format 'flac' (allowed: wav, mp3, pcm16)")
  assert.equal(metadata(format('flac', 'cli')).error, "Unsupported speech format 'flac' (allowed: wav, mp3, pcm16)")
})
test('surface-specific failures preserve original CLI and tool messages', () => {
  for (const surface of ['cli', 'tool']) {
    const tool = surface === 'tool'
    assert.equal(metadata(options({ surface, has_clone_path: true, has_voice: true })).error, tool ? 'use either clone_voice or voice for cloned voice data, not both' : 'Use either --clone-voice or --voice for cloned voice data, not both')
    assert.equal(metadata(options({ surface, model: 'mimo-v2.5-tts-voiceclone' })).error, tool ? 'mimo-v2.5-tts-voiceclone requires clone_voice <mp3|wav> or voice <data-uri>' : 'mimo-v2.5-tts-voiceclone requires --clone-voice <mp3|wav> or --voice <data-uri>')
    assert.equal(metadata(options({ surface, voice_prompt: '' })).error, tool ? 'mimo-v2.5-tts-voicedesign requires voice_prompt or instruction' : 'mimo-v2.5-tts-voicedesign requires --voice-prompt or --instruction to describe the voice')
    assert.equal(transformStockSnapshot(options({ surface, model: 'mimo-chat' })).result.success, false)
  }
})
test('empty CLI voice presence still conflicts with clone and otherwise defaults', () => {
  assert.equal(metadata(options({ surface: 'cli', has_voice: true })).voice, 'default')
  assert.equal(transformStockSnapshot(options({ surface: 'cli', has_voice: true, has_clone_path: true })).result.success, false)
})
test('malformed, contradictory and oversized snapshots refuse', () => {
  for (const input of [options({ surface: 'daemon' }), options({ has_voice: false, voice_is_data_uri: true }), options({ voice_nonempty: true }), options({ has_voice: 'true' }), options({ model: 42 }), options({ instruction: 'x'.repeat(1024 * 1024) }), format(null)]) assert.throws(() => transformStockSnapshot(input))
})

test('actual pinned harness redeems one captured speech plan without sample/path authority', async () => {
  const owner = { plugin_id: 'host:harness', generation: 8, owner_token: 'fixture-owner' }
  const calls = []
  const runner = createHarnessModule({ async request(method, input) { calls.push([method, input]); return options({ has_voice: true, voice_nonempty: true, voice_is_data_uri: true }) } }, owner)
  const plan = await runner.run({ owner, execution_id: 'captured-speech', ticket: 'fixture-ticket', deadline_ms: 1000 }, new AbortController().signal)
  assert.deepEqual(plan.result.metadata, { model: 'mimo-v2.5-tts-voiceclone', instruction: null, voice: 'raw' })
  assert.deepEqual(calls, [['exec/redeem', { owner, execution_id: 'captured-speech', ticket: 'fixture-ticket' }]])
  assert.equal(JSON.stringify(plan).includes('base64'), false)
  await runner.dispose()
})
test('speech runner cancellation settles before a late reply and never retries', async () => {
  const owner = { plugin_id: 'host:harness', generation: 8, owner_token: 'fixture-owner' }
  const abort = new AbortController()
  let finish, calls = 0
  const runner = createHarnessModule({ request() { calls++; return new Promise(resolve => { finish = resolve }) } }, owner)
  const pending = runner.run({ owner, execution_id: 'captured-speech', ticket: 'fixture-ticket', deadline_ms: 1000 }, abort.signal)
  abort.abort(); await assert.rejects(pending, /cancelled/)
  finish(options({})); assert.equal(calls, 1); await runner.dispose()
})
