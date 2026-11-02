export const inject = ['tools']
export function apply(ctx) {
  ctx.tools.register({ name: 'crash_probe', description: 'Terminate this fixture host.',
    parameters: { type: 'object', properties: {} },
    execute() { process.kill(process.pid, 'SIGKILL') },
  })
}
