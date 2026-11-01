// Slash commands for the extension-host tests: text, a submitted prompt, an
// error, escape sequences in output, a wait that honours cancellation, and
// both the Codewhale-native and the DSH-compatible registration shapes.
export const name = 'ext-commands'
export const inject = ['commands']

export function apply(ctx) {
  ctx.commands.register({
    name: 'ext-echo',
    description: 'Echo the arguments.',
    argumentHint: '<text>',
    handler: ({ args }) => ({ kind: 'success', text: `echo: ${args}` }),
  })
  ctx.commands.register({
    name: 'ext-ask',
    description: 'Submit a prompt built from the arguments.',
    input: { hint: '<topic>' },
    handler: ({ args }) => ({ kind: 'submit', prompt: `Summarize: ${args}`, text: 'Asking the model.' }),
  })
  ctx.commands.register({
    name: 'ext-fail',
    description: 'Report an error.',
    handler: () => ({ kind: 'error', text: 'unknown topic' }),
  })
  ctx.commands.register({
    name: 'ext-throw',
    description: 'Throw from the handler.',
    handler: () => {
      throw new Error('boom')
    },
  })
  ctx.commands.register({
    name: 'ext-ansi',
    description: 'Return terminal escape sequences.',
    handler: () => 'plain \u001b[31mred\u001b[0m \u001b]0;title\u0007end',
  })
  // DSH shape: `rawInput` carries its leading separator.
  ctx.commands.register({
    name: 'ext-dsh',
    description: 'DSH-style handler.',
    input: { hint: '<x>' },
    handler: ({ rawInput }) => ({ kind: 'success', text: JSON.stringify(rawInput) }),
  })
  ctx.commands.register({
    name: 'ext-slow',
    description: 'Wait for the given milliseconds (default 30000), or until cancelled.',
    argumentHint: '<ms>',
    handler: ({ args, signal }) =>
      new Promise((resolve, reject) => {
        const timer = setTimeout(() => resolve({ kind: 'success', text: 'waited' }), Number(args) || 30000)
        signal.addEventListener('abort', () => {
          clearTimeout(timer)
          reject(new Error('aborted'))
        })
      }),
  })
}
