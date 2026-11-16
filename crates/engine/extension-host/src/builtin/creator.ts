import { apply as applyPluginManager } from '../dsh/upstream/creator/plugin-manager/tools.js'
import * as toolCordis from '../dsh/upstream/creator/tool-cordis/index.js'
import { createCordisInspect } from '../dsh/inspect.ts'

export const name = 'creator'
export const inject = ['tools', 'pluginManager', 'sandboxPolicy']

export function apply(ctx: any) {
  applyPluginManager(ctx)
  const cordisInspect = createCordisInspect({
    installed: async (signal) => {
      const rows = (await ctx.pluginManager.listPlugins(signal)) as any[]
      return rows.map((r) => ({
        id: String(r.entryId),
        name: String(r.moduleName),
        state: !r.enabled ? 'disabled' : String(r.fiberPhase ?? 'active'),
      }))
    },
    tools: async (_agent, signal) => {
      const names = (await ctx.pluginManager.listTools(signal)) as any[]
      return names.map((name) => ({ name: String(name) }))
    },
  })
  ctx.provide('cordisInspect', cordisInspect)
  ctx.plugin(toolCordis)
}
