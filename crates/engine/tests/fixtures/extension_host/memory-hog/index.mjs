export const inject = ['tools']
export function apply(ctx) {
  ctx.tools.register({ name: 'memory_hog', description: 'Allocate and touch memory, 64 MiB at a time.',
    parameters: { type: 'object', properties: { mib: { type: 'number' } } },
    async execute(args) {
      // Buffers live outside the JS heap, so Node's --max-old-space-size does
      // not bound them: only the OS-level cap does.
      const chunks = []
      while (chunks.length * 64 < args.mib) {
        chunks.push(Buffer.alloc(64 * 1024 * 1024, 1))
        await new Promise((resolve) => setTimeout(resolve, 5))
      }
      return { allocated_mib: chunks.length * 64 }
    },
  })
}
