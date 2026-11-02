// Tools that use `exec.core.call` and report exactly what came back, so a test
// can see every outcome the core can produce: a result, or a typed refusal.
export const name = 'core-call'
export const inject = ['tools', 'commands']

const outcome = async (promise) => {
  try {
    return { ok: await promise }
  } catch (error) {
    return { failed: { name: error.name, code: error.code ?? null, message: error.message } }
  }
}

export function apply(ctx) {
  // One core call: `{name, input}` -> `{ok: {content, isError, structured?}}` or `{failed: {code, message}}`.
  ctx.tools.register({
    name: 'cc_call',
    description: 'Ask the core to run a tool and report the outcome.',
    parameters: {
      type: 'object',
      properties: { name: { type: 'string' }, input: { type: 'object' } },
      required: ['name'],
      additionalProperties: false,
    },
    execute: async ({ name, input }, exec) => {
      if (exec.core === undefined) return { noCore: true }
      return outcome(exec.core.call(name, input ?? {}))
    },
  })

  // `count` calls of the same tool, one after another or all at once.
  ctx.tools.register({
    name: 'cc_many',
    description: 'Ask the core to run a tool several times and report every outcome.',
    parameters: {
      type: 'object',
      properties: { name: { type: 'string' }, input: { type: 'object' }, count: { type: 'number' }, parallel: { type: 'boolean' } },
      required: ['name', 'count'],
      additionalProperties: false,
    },
    execute: async ({ name, input, count, parallel }, exec) => {
      if (exec.core === undefined) return { noCore: true }
      const one = () => outcome(exec.core.call(name, input ?? {}))
      if (parallel) return { outcomes: await Promise.all(Array.from({ length: count }, one)) }
      const outcomes = []
      for (let i = 0; i < count; i++) outcomes.push(await one())
      return { outcomes }
    },
  })

  ctx.tools.register({
    name: 'cc_probe',
    description: 'Report whether this call has exec.core, and what exec holds.',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    execute: (_input, exec) => ({ hasCore: 'core' in exec, keys: Object.keys(exec).sort(), coreKeys: exec.core ? Object.keys(exec.core).sort() : null }),
  })

  // A bad request the host refuses before sending anything.
  ctx.tools.register({
    name: 'cc_local',
    description: 'Make core calls the host itself refuses to send.',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    execute: async (_input, exec) => ({
      emptyName: await outcome(exec.core.call('')),
      longName: await outcome(exec.core.call('x'.repeat(200))),
      notJson: await outcome(exec.core.call('read', { fn: () => 1 })),
      cyclic: await outcome(exec.core.call('read', (() => { const a = {}; a.a = a; return a })())),
    }),
  })

  // Sleeps after (or without) a core call; for call-deadline tests.
  ctx.tools.register({
    name: 'cc_then_sleep',
    description: 'Make one core call, then wait `ms` milliseconds or until cancelled.',
    parameters: {
      type: 'object',
      properties: { name: { type: 'string' }, input: { type: 'object' }, ms: { type: 'number' } },
      required: ['name', 'ms'],
      additionalProperties: false,
    },
    execute: async ({ name, input, ms }, exec) => {
      const first = await outcome(exec.core.call(name, input ?? {}))
      await new Promise((resolve, reject) => {
        const timer = setTimeout(resolve, ms)
        exec.signal.addEventListener('abort', () => {
          clearTimeout(timer)
          reject(new Error('aborted'))
        })
      })
      return { first }
    },
  })

  ctx.commands.register({
    name: 'cc-probe',
    description: 'Report whether a command invocation has core.',
    handler: (invocation) => ({ kind: 'success', text: JSON.stringify({ hasCore: 'core' in invocation, keys: Object.keys(invocation).sort() }) }),
  })
}
