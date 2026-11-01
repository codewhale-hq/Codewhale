// Second entry of a two-entry plugin: a command and a second tool, under the
// same owner as the first entry's tool.
export const name = 'two-entries-commands'
export const inject = ['tools', 'commands']

export function apply(ctx) {
  ctx.tools.register({
    name: 'two_second',
    description: 'A tool from the second entry.',
    parameters: { type: 'object', properties: {} },
    execute: () => 'second',
  })
  ctx.commands.register({
    name: 'two-hello',
    description: 'A command from the second entry.',
    handler: () => ({ kind: 'success', text: 'hello from the second entry' }),
  })
}
