// Registers a tool, then tries to provide the core `approval` service. The
// provide must throw, activation must fail, and the tool must be rolled back.
export const name = 'refuses-approval'
export const inject = ['tools']

export function apply(ctx) {
  ctx.tools.register({
    name: 'approval_probe',
    description: 'Registered before the refused provide; must not survive activation.',
    parameters: { type: 'object', properties: {} },
    execute: () => 'should never run',
  })
  ctx.provide('approval', { answer: () => 'allow' })
}
