// Reports what the host gives a plugin: its settings (validated by this
// plugin's own `Config` schema, with defaults), and, per call, the workspace
// and its own data directory.
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import Schema from '@deepseek-ai/schemastery'

export const name = 'plugin-context'
export const inject = ['tools', 'commands']

export const Config = Schema.object({
  greeting: Schema.string().default('Hello'),
  limit: Schema.number().min(1).max(10).default(3),
})

export function apply(ctx, config) {
  ctx.tools.register({
    name: 'ctx_probe',
    description: 'Report the plugin context.',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    execute: (_input, exec) => ({ config, workspace: exec.workspace ?? null, dataDir: exec.dataDir ?? null, keys: Object.keys(exec).sort() }),
  })
  ctx.tools.register({
    name: 'ctx_note',
    description: 'Write a note into the plugin data directory and read it back.',
    parameters: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'], additionalProperties: false },
    async execute({ text }, exec) {
      await mkdir(exec.dataDir, { recursive: true })
      const file = join(exec.dataDir, 'note.txt')
      await writeFile(file, text)
      return { file, text: await readFile(file, 'utf8') }
    },
  })
  ctx.tools.register({
    name: 'ctx_frozen',
    description: 'Try to change the call context.',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    execute: (_input, exec) => {
      let changed = false
      try {
        exec.workspace = '/elsewhere'
        changed = exec.workspace === '/elsewhere'
      } catch {
        // frozen objects throw in strict mode
      }
      return { changed }
    },
  })
  ctx.commands.register({
    name: 'ctx-probe',
    description: 'Report the plugin context.',
    handler: (invocation) =>
      ({ kind: 'success', text: JSON.stringify({ config, workspace: invocation.workspace ?? null, dataDir: invocation.dataDir ?? null }) }),
  })
}
