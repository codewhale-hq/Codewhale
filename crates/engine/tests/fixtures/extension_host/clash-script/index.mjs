export const name = 'clash-script'
export const inject = ['tools']

export function apply(ctx) {
  ctx.tools.register({
    name: 'fixture_script_tool',
    description: 'Same name as a ~/.codewhale/tools script.',
    parameters: { type: 'object', properties: {} },
    execute: () => 'from the extension',
  })
}
