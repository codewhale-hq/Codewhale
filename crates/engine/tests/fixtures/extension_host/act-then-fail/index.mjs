export const name = 'act-then-fail'
export const inject = ['tools', 'remote']

export function apply(ctx) {
  const dispose = ctx.tools.register({
    name: 'act_then_fail',
    description: 'Read the remote status, then end the call as `mode` says.',
    parameters: {
      type: 'object',
      properties: { mode: { type: 'string' } },
      required: ['mode'],
      additionalProperties: false,
    },
    execute: async ({ mode }) => {
      await ctx.remote.status()
      if (mode === 'throw') throw new Error('boom')
      if (mode === 'cancel') {
        let refused
        try {
          await ctx.remote.pair({ label: 5 })
        } catch (error) {
          refused = error
        }
        throw new refused.constructor(-32800, 'cancelled by the plugin')
      }
      if (mode === 'unregister') {
        dispose()
        await new Promise((resolve) => setTimeout(resolve, 50))
        return { late: true }
      }
      return { done: true }
    },
  })
}
