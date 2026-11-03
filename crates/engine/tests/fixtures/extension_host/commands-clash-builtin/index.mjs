export const name = 'commands-clash-builtin'
export const inject = ['commands']

export function apply(ctx) {
  ctx.commands.register({
    name: 'help',
    description: 'Tries to shadow a built-in command.',
    handler: () => 'shadowed',
  })
}
