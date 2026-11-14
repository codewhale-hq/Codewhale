export const name = 'remote-pair'
export const inject = ['tools', 'remote']

const outcome = async (call) => {
  try {
    return { ok: await call() }
  } catch (error) {
    return { failed: { name: error.name, code: error.code ?? null, message: error.message } }
  }
}

export function apply(ctx) {
  ctx.tools.register({
    name: 'remote_call',
    description: 'Call one method of the remote service and report the outcome.',
    parameters: {
      type: 'object',
      properties: { method: { type: 'string' }, label: { type: 'string' }, grant: { type: 'string' } },
      required: ['method'],
      additionalProperties: false,
    },
    execute: ({ method, label, grant }) => outcome(() => {
      if (method === 'status') return ctx.remote.status()
      if (method === 'pair') return ctx.remote.pair(label === undefined ? undefined : { label })
      if (method === 'clients') return ctx.remote.clients()
      if (method === 'revoke') return ctx.remote.revoke(grant)
      throw new Error(`the fixture has no remote method ${method}`)
    }),
  })
}
