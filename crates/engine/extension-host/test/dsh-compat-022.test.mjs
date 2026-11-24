import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { activate, startHost } from './harness.mjs'

function tempPlugin(source) {
  const dir = mkdtempSync(join(tmpdir(), 'cw-dsh-022-'))
  const entry = join(dir, 'index.mjs')
  const dataDir = join(dir, 'data')
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(entry, source)
  return { dir, entry, dataDir, cleanup: () => rmSync(dir, { recursive: true, force: true }) }
}

test('DSH 0.2.2 pure-API exports and scoped module shims work and enforce dataDir confinement', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())

  const plugin = tempPlugin(`
import assert from 'node:assert/strict'
import { join } from 'node:path'
import { ReasoningEffortId, BlockAssembler, createAssistantMessage, contentHasImage } from '@deepseek-ai/dsh-llm'
import { SessionId, KNOWN_SESSION_EVENT_TYPES, deriveEventMessage } from '@deepseek-ai/dsh-session'
import { credentialRef } from '@deepseek-ai/dsh-credentials'
import { SettingsConflictError, settingsNamespace } from '@deepseek-ai/dsh-settings'
import { resolveDshHome } from '@deepseek-ai/dsh-home-paths'
import { writeFileAtomic, withFileLock } from '@deepseek-ai/dsh-atomic-write'
import { BUNDLED_SKILL_RANK, isModelInvocable, isUserInvocable, renderSkillContent } from '@deepseek-ai/dsh-skill'
import { Remote, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol'

export const name = 'dsh-shims-probe'
export const inject = ['tools', 'commands', 'settings', 'credentials', 'agentPresets', 'agentDefaultModel', 'sessionProjections', 'workspaceRegistry', 'typertGateway', 'web']
export async function apply(ctx) {
  assert.equal(ReasoningEffortId('high'), 'high')
  const asm = new BlockAssembler()
  asm.push({ type: 'text_delta', text: 'hel' })
  asm.push({ type: 'text_delta', text: 'lo' })
  assert.deepEqual(asm.finish(), [{ type: 'text', text: 'hello' }])
  assert.equal(createAssistantMessage({ content: [{ type: 'text', text: 'hi' }] }).role, 'assistant')
  assert.equal(contentHasImage([{ type: 'image', attachment: {} }]), true)

  assert.equal(SessionId('s1'), 's1')
  assert.ok(KNOWN_SESSION_EVENT_TYPES instanceof Set)
  assert.ok(KNOWN_SESSION_EVENT_TYPES.has('message/user'))
  const derived = deriveEventMessage({ type: 'message/user', text: 'hello' })
  assert.equal(derived.role, 'user')
  assert.equal(derived.content[0].text, 'hello')

  assert.equal(credentialRef('  openai_key  '), 'openai_key')
  assert.throws(() => credentialRef(''), TypeError)

  assert.equal(settingsNamespace('my-ns'), 'my-ns')
  const conflict = new SettingsConflictError('my-ns', 'r1', 'r2')
  assert.equal(conflict.code, 'SETTINGS_CONFLICT')

  assert.equal(typeof BUNDLED_SKILL_RANK, 'number')
  assert.equal(isModelInvocable({ disableModelInvocation: true }), false)
  assert.equal(isUserInvocable({}), true)
  assert.equal(renderSkillContent('---\\nname: demo\\n---\\nRun $ARGUMENTS', 'fast'), 'Run fast')

  assert.equal(typeof Remote.strictCodec, 'function')
  assert.equal(typeof TypertRemoteService, 'function')

  const home = resolveDshHome()
  const nested = resolveDshHome('sub', 'state.json')
  assert.equal(nested, join(home, 'sub', 'state.json'))
  assert.throws(() => resolveDshHome('..', 'escape.txt'), /escaping plugin dataDir/)
  assert.throws(() => resolveDshHome('/etc/passwd'), /absolute or invalid/)

  await withFileLock(nested, async () => {
    await writeFileAtomic(nested, JSON.stringify({ ok: true }))
  })
  await assert.rejects(writeFileAtomic(join(home, '..', 'escape.txt'), 'nope'), /outside plugin dataDir/)

  const disposePreset = ctx.agentPresets.register({ id: 'p1', name: 'Preset 1' })
  assert.equal((await ctx.agentPresets.list()).length, 1)
  disposePreset()
  assert.equal((await ctx.agentPresets.list()).length, 0)

  const disposeGw = ctx.typertGateway.register('demo', { ping: () => 'pong' }, [])
  assert.deepEqual(ctx.typertGateway.list(), ['demo'])
  disposeGw()

  const disposeSearch = ctx.web.registerSearchProvider({ id: 'custom-search', name: 'Custom' })
  assert.equal(ctx.web.listSearchProviders().length, 1)
  disposeSearch()

  assert.throws(() => ctx.workspaceRegistry.create(), /read-only/)

  ctx.commands.register({
    name: 'session-probe',
    description: 'verify agent.session',
    handler(inv) {
      return { kind: 'success', text: inv.agent.session.id + ':' + (inv.agent.session.header.cwd ?? '') }
    },
  })
}
`)
  t.after(() => plugin.cleanup())

  const { ref, result } = await activate(host, 'dsh-shims-probe', plugin.entry, { data_dir: plugin.dataDir })
  assert.equal(result.status, 'ok', result.diagnostic)
  const written = JSON.parse(readFileSync(join(plugin.dir, 'data', 'sub', 'state.json'), 'utf8'))
  assert.deepEqual(written, { ok: true })

  const cmdReg = host.registry.find((entry) => entry.op === 'register' && entry.kind === 'command' && entry.spec.name === 'session-probe')
  assert.ok(cmdReg)
  const cmdOut = await host.call('command/run', {
    handle: cmdReg.handle,
    raw_input: '',
    command_id: 'cmd-1',
    workspace: '/tmp/ws',
    session_id: 'sess-42',
    deadline_ms: 5000,
  })
  assert.deepEqual(cmdOut, { kind: 'success', text: 'sess-42:/tmp/ws' })

  assert.deepEqual(await host.call('ext/deactivate', { owner: ref }), { disposed: true, leaked: [] })
})

test('tools.guard is deny-only, subagents.getProvider("fork") works, tools/result projects, and approval/request is refused', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())

  const plugin = tempPlugin(`
import assert from 'node:assert/strict'
export const name = 'guard-and-fork'
export const inject = ['tools', 'subagents']
export function apply(ctx) {
  const fork = ctx.subagents.getProvider('fork')
  assert.ok(fork)
  assert.equal(fork.inheritsParentContext, true)
  assert.equal(fork.capabilities.depthLimit, true)

  ctx.tools.guard((exec) => {
    if (exec.name === 'blocked_tool') return 'blocked by policy'
    if (exec.name === 'try_allow') return { kind: 'allow' }
    return undefined
  })

  ctx.on('tools/result', () => {})
}
`)
  t.after(() => plugin.cleanup())

  const { ref, result } = await activate(host, 'guard-and-fork', plugin.entry)
  assert.equal(result.status, 'ok', result.diagnostic)

  const preHook = host.registry.find((entry) => entry.op === 'register' && entry.kind === 'hook' && entry.spec.name === 'tools/pre-execute')
  const postHook = host.registry.find((entry) => entry.op === 'register' && entry.kind === 'hook' && entry.spec.name === 'tools/post-execute')
  assert.ok(preHook)
  assert.ok(postHook)

  const denyVerdict = await host.call('hook/evaluate', {
    handle: preHook.handle,
    event: 'tools/pre-execute',
    payload: { name: 'blocked_tool', call_id: 'c1', input: {}, workspace: '/tmp', mode: 'act', model: 'm' },
    deadline_ms: 5000,
  })
  assert.deepEqual(denyVerdict, { kind: 'deny', reason: 'blocked by policy' })

  const abstainVerdict = await host.call('hook/evaluate', {
    handle: preHook.handle,
    event: 'tools/pre-execute',
    payload: { name: 'try_allow', call_id: 'c2', input: {}, workspace: '/tmp', mode: 'act', model: 'm' },
    deadline_ms: 5000,
  })
  assert.deepEqual(abstainVerdict, { kind: 'abstain' })

  assert.deepEqual(await host.call('ext/deactivate', { owner: ref }), { disposed: true, leaked: [] })

  const badEvent = tempPlugin(`
export const name = 'bad-approval-listener'
export function apply(ctx) {
  ctx.on('approval/request', () => {})
}
`)
  t.after(() => badEvent.cleanup())
  const refused = (await activate(host, 'bad-approval-listener', badEvent.entry)).result
  assert.equal(refused.status, 'failed')
  assert.match(refused.diagnostic, /DSH lifecycle approval\/request is not projected/)
})
