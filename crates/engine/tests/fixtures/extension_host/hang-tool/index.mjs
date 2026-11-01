export const inject = ['tools']
export function apply(ctx) {
  ctx.tools.register({ name: 'hang_probe', description: 'Block the fixture event loop.',
    parameters: { type: 'object', properties: { ms: { type: 'number' } } },
    execute(args) {
      const end = args.ms === undefined ? Infinity : Date.now() + args.ms
      while (Date.now() < end) {}
      return { resumed: true }
    },
  })
}
