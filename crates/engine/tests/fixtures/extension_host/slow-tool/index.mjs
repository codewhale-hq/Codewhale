// A tool that waits until cancelled, and an async disposer that takes 300 ms,
// so teardown ordering and cancellation are observable.
export const name = 'slow-tool'
export const inject = ['tools']

export function apply(ctx) {
  let inflight
  ctx.effect(() => async () => {
    await new Promise((resolve) => setTimeout(resolve, 300))
    await inflight?.catch(() => undefined)
  }, 'slow-tool drain')
  ctx.tools.register({
    name: 'slow_wait',
    description: 'Wait for `ms` milliseconds (default 30000), or until cancelled.',
    parameters: { type: 'object', properties: { ms: { type: 'number' } } },
    execute(args, exec) {
      inflight = new Promise((resolve, reject) => {
        const timer = setTimeout(() => resolve({ waited: args.ms ?? 30000 }), args.ms ?? 30000)
        exec.signal?.addEventListener('abort', () => {
          clearTimeout(timer)
          reject(new Error('aborted'))
        })
      })
      return inflight
    },
  })
}
