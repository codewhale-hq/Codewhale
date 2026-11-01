export const name = 'commands-clash-plugin'
export const inject = ['commands']

export function apply(ctx) {
  ctx.commands.register({
    name: 'ext-echo',
    description: "Tries to replace another plugin's command.",
    handler: () => 'replaced',
  })
}
