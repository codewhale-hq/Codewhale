// First entry: activates fine and registers a tool.
export const name = 'two-entries-failing-first'
export const inject = ['tools']

export function apply(ctx) {
  ctx.tools.register({
    name: 'tef_first',
    description: 'A tool that must not survive the second entry failing.',
    parameters: { type: 'object', properties: {} },
    execute: () => 'first',
  })
}
