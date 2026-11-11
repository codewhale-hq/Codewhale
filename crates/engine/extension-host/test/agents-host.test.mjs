import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { activate, startHost } from './harness.mjs'
import { ErrorCode } from '../dist/protocol.mjs'

const PLUGIN = `
export const inject = ['tools', 'subagents', 'agents']
export function apply(ctx) {
  ctx.on('agent/created', ({ agent }) => {
    agent.ctx.systemPrompt.section({ name: 'deployment:persona-prefix', order: agent.ctx.systemPrompt.getSectionOrder('DEPLOYMENT_PERSONA_PREFIX'), text: 'Reviewer persona' })
    agent.ctx.tools.restrict({ deny: ['exec_shell'] })
  })
  ctx.tools.register({ name: 'team_start', description: 'Start one child agent.', parameters: { type: 'object' },
    async execute(input, exec) {
      try {
        const parent = input.forged ? { id: exec.agent?.id } : exec.agent
        const handle = await ctx.subagents.startActivation({
          provider: 'spawn', childId: input.childId ?? 'reviewer-1', delivery: 'caller',
          request: { parent, prompt: [{ type: 'text', text: 'Review the diff' }] },
        })
        const result = await handle.result
        await handle.dispose()
        return { messageId: handle.messageId, stopReason: result.stopReason, text: result.output.map(block => block.text).join('') }
      } catch (error) {
        return { refused: error.code ?? String(error) }
      }
    },
  })
}
`

test('the agents adapters serve subagents and agents through the reviewed adapter table', async (t) => {
  const host = await startHost({
    subagent: (params, _id, method) => method === 'subagent/start'
      ? { child_id: params.child_id, message_id: 'message-1' }
      : method === 'subagent/result'
        ? { stop_reason: 'end_turn', output: [{ type: 'text', text: 'looks good' }], omitted_blocks: 0 }
        : {},
  })
  t.after(() => host.stop())
  const dir = mkdtempSync(join(tmpdir(), 'cw-agents-'))
  t.after(() => rmSync(dir, { recursive: true, force: true }))
  const entry = join(dir, 'index.mjs')
  writeFileSync(entry, PLUGIN)
  const { ref, result } = await activate(host, 'agent-team', entry)
  assert.deepEqual(result, { status: 'ok', tools: ['team_start'], commands: [] })
  const handle = host.registry.find(row => row.op === 'register' && row.spec.name === 'team_start').handle
  const call = (input, identity) => host.call('tool/call', { handle, call_id: 'team', input, deadline_ms: 5000, ...identity })
  const root = { session_id: 'session-root', workspace: '/workspace/team', subagent_ticket: 'subagent-ticket-1' }

  assert.deepEqual((await call({}, root)).structured, { messageId: 'message-1', stopReason: 'end_turn', text: 'looks good' })
  assert.deepEqual(host.subagents.map(row => row.method), ['subagent/start', 'subagent/result', 'subagent/dispose'])
  const [start] = host.subagents
  assert.deepEqual(start.owner, ref)
  assert.equal(start.ticket, 'subagent-ticket-1')
  assert.equal(start.child_id, 'reviewer-1')
  assert.equal(start.prompt, 'Review the diff')
  assert.equal(start.persona, 'Reviewer persona')
  assert.deepEqual(start.denied_tools, ['exec_shell'])
  assert.equal(start.delivery, 'caller')
  assert.ok(host.subagents.every(row => row.ticket === 'subagent-ticket-1' && row.child_id === 'reviewer-1'))

  const before = host.subagents.length
  assert.deepEqual((await call({}, { ...root, agent_id: 'agent-child' })).structured, { refused: ErrorCode.NotAvailable }, 'a delegated caller cannot nest child agents')
  assert.deepEqual((await call({}, { session_id: 'session-root', workspace: '/workspace/team' })).structured, { refused: ErrorCode.NotAvailable }, 'no subagent ticket, no child')
  assert.deepEqual((await call({ forged: true }, root)).structured, { refused: ErrorCode.NotAvailable }, 'a forged parent is not the live caller')
  assert.deepEqual((await call({ childId: 'bad id' }, root)).structured, { refused: ErrorCode.InvalidParams })
  assert.equal(host.subagents.length, before, 'refused activations never reach the core')
})
