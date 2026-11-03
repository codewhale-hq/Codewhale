export const inject = ['tools', 'commands', 'prompt', 'skills', 'storage']
export function apply(ctx, config) {
  ctx.prompt.registerSection({ id: 'composition-note', text: config.text })
  ctx.skills.registerRoot({ path: 'source/skills' })
  ctx.commands.register({ name: 'composition-echo', description: 'Show the composed text.', handler: () => ({ kind: 'success', text: config.text }) })
  ctx.tools.register({
    name: 'composition_echo', description: 'Echo composed text through the existing approval gate.',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    execute: async () => ({ text: config.text, count: (await ctx.storage.get('composition-example-count')) ?? null }),
  })
}
