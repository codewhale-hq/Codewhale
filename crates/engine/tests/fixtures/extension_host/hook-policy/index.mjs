export const name = 'hook-policy'
export const inject = ['prompt']

export function apply(ctx) {
  ctx.prompt.registerSection({ id: 'repo-style', text: 'Use the repository style guide when preparing release notes.' })
  ctx.on('tools/pre-execute', async (exec, next) => {
    if (!Object.isFrozen(exec) || !Object.isFrozen(exec.arguments) || exec.core !== undefined) {
      throw new Error('hook execution view is not frozen or has core authority')
    }
    switch (exec.arguments.path) {
      case 'before.txt': return { kind: 'revise', input: { ...exec.arguments, path: 'after.txt' } }
      case 'blocked.txt': return { kind: 'deny', reason: 'blocked by fixture' }
      case 'ask.txt': return { kind: 'ask', reason: 'fixture asks' }
      case 'allow.txt': return { kind: 'allow' }
      case 'context.txt': return { kind: 'annotate', text: 'fixture context' }
      case 'malformed.txt': return { kind: 'revise', input: [] }
      case 'throw.txt': throw new Error('fixture hook failure')
      case 'timeout.txt':
      case 'held.txt':
        await new Promise((resolve) => exec.signal.addEventListener('abort', resolve, { once: true }))
        return { kind: 'revise', input: { path: 'after.txt' } }
      case 'rewrite-write.txt': return { kind: 'revise', input: { ...exec.arguments, path: '../escaped.txt' } }
      case 'rewrite-action.txt': return { kind: 'revise', input: { action: 'write', path: 'rewritten.txt', content: 'requires fresh approval' } }
      default: return next()
    }
  })
}
