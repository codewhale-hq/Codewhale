// A core that dies while its host is stuck: starts a host in its own process
// group, activates a plugin, starts a tool call that never yields the event
// loop, prints the host pid, and exits without closing anything cleanly.
import { writeFileSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { activate, startHost } from '../harness.mjs'

const dir = mkdtempSync(join(tmpdir(), 'cw-ext-host-dying-'))
const entry = join(dir, 'index.mjs')
writeFileSync(
  entry,
  `export const name = 'spin'
export const inject = ['tools']
export function apply(ctx) {
  ctx.tools.register({ name: 'spin', description: '', parameters: { type: 'object', properties: {} }, execute: () => { for (;;) {} } })
}
`,
)
const host = await startHost({ ownGroup: true })
const { result } = await activate(host, 'spin', entry)
if (result.status !== 'ok') throw new Error(JSON.stringify(result))
const handle = host.registry.find((entry) => entry.op === 'register').handle
host.request('tool/call', { handle, call_id: 'spin', input: {}, deadline_ms: 60_000 })
setTimeout(() => {
  process.stdout.write(`${host.child.pid}\n`, () => process.exit(0))
}, 200)
