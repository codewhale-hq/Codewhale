// First entry of a two-entry plugin: a tool.
export const name = 'two-entries-tools'
export const inject = ['tools']

export function apply(ctx) {
  ctx.tools.register({
    name: 'two_first',
    description: 'A tool from the first entry.',
    parameters: { type: 'object', properties: {} },
    execute: () => 'first',
  })
}
