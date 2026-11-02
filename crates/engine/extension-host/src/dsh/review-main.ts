/** Detached trusted offline reviewer. No HostRoot, plugin import or expression evaluation. */
import { reviewComposition, type ReviewedComposition } from './composition.ts'
import {reviewAgentPresets, type PresetReviewInput} from './agent-presets.ts'

try {
  let bytes = 0
  const chunks: Buffer[] = []
  for await (const chunk of process.stdin) {
    const part = Buffer.from(chunk)
    bytes += part.length
    if (bytes > 8 * 1024 * 1024) throw new Error('review request exceeds 8 MiB')
    chunks.push(part)
  }
  const input = JSON.parse(Buffer.concat(chunks).toString('utf8')) as ReviewedComposition
  const output = JSON.stringify((input as any).kind === 'agent-presets' ? await reviewAgentPresets(input as unknown as PresetReviewInput) : reviewComposition(input))
  if (Buffer.byteLength(output) > 8 * 1024 * 1024) throw new Error('review response exceeds 8 MiB')
  process.stdout.write(output)
} catch {
  // Parser diagnostics can contain source literals. Emit a source-free error.
  process.stderr.write('DSH composition review refused invalid or oversized data.\n')
  process.exitCode = 1
}
