export const name = 'dirty-dispose'

export function apply(ctx) {
  // Keep the event loop responsive while the existing disposal deadline runs.
  ctx.effect(() => () => new Promise(() => {}), 'unfinished fixture disposal')
}
