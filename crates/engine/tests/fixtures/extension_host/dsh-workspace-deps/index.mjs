// Codewhale host entry: a Cordis plugin that loads the published, unmodified
// @deepseek-ai/dsh-tool-workspace-dependencies@0.1.7-alpha.2 lib/index.js with
// literal config — the same semantics as one literal DSH bundle patch row.
import { fileURLToPath } from 'node:url'
import * as workspaceDependencies from './vendor/dsh-tool-workspace-dependencies/lib/index.js'

export const name = 'dsh-workspace-deps'

export async function apply(ctx) {
  const source = fileURLToPath(new URL(`./payload/${process.platform}-${process.arch}`, import.meta.url))
  await ctx.plugin(workspaceDependencies, { source })
}
