/** Presentation of immutable Core review facts. No provider, I/O, pass or publication authority. */
import { isJson } from '../../json.ts'

type Row = Record<string, unknown>
interface Result { ok: true; result: { content: string; success: true; metadata: null } }
export const REVIEW_LIMIT = 16 * 1024 * 1024
const OPERATIONS = ['review_source_prompt', 'review_pass_prompt', 'review_interactive_pr', 'review_report']
export function isReviewOperation(value: unknown): value is string { return typeof value === 'string' && OPERATIONS.includes(value) }
export function reviewEnvelopeBytes(value: unknown): number { return 8 + Buffer.byteLength(`{"jsonrpc":"2.0","id":18446744073709551615,"result":${JSON.stringify(value)}}`) }
function row(value: unknown): Row { if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('invalid Core review snapshot'); return value as Row }
function text(value: unknown): string { if (typeof value !== 'string') throw new Error('invalid Core review text'); return value }
function bool(value: unknown): boolean { if (typeof value !== 'boolean') throw new Error('invalid Core review flag'); return value }
function integer(value: unknown): number { if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error('invalid Core review count'); return value as number }
function list(value: unknown): unknown[] { if (!Array.isArray(value)) throw new Error('invalid Core review list'); return value }
function optional(value: unknown): string | undefined { return value === null || value === undefined ? undefined : text(value) }
function trim(value: string): string { return value.replace(/^[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+|[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$/g, '') }
function lines(value: string): string[] {
  if (!value.length) return []
  const out = value.split('\n')
  if (value.endsWith('\n')) out.pop()
  return out.map((line, i) => (i < out.length - 1 || value.endsWith('\n')) && line.endsWith('\r') ? line.slice(0, -1) : line)
}
function skipped(value: unknown): string { return list(value).map(value => { const skip = row(value); return `${text(skip.file)} (${integer(skip.chars)} chars; ${text(skip.reason)})` }).join(', ') }
function ordered(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(ordered)
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, value]) => [key, ordered(value)]))
  return value
}
function intent(value: unknown): string {
  const commits = value === undefined ? '' : text(value)
  return commits.trim() ? `The author's commit messages for this range follow. They state what the change is meant to do; check them against the diff rather than trusting them.\n\n${commits}\n\nEnd of commit messages.\n\n` : ''
}
function source(input: Row): string {
  const kind = text(input.kind)
  if (kind === 'file') {
    const content = lines(text(input.content)).map((line, i) => `${String(i + 1).padStart(4)} | ${line}`).join('\n')
    return `Review the following file and provide feedback.\nPath: ${text(input.display)}\n\n${content}\n\nEnd of file.`
  }
  if (kind === 'diff') return `Review the following ${text(input.label)} and provide feedback.\n\n${text(input.diff)}\n\nEnd of diff.`
  if (kind === 'cli_diff') return `${intent(input.commits)}Review the following diff and provide feedback:\n\n${text(input.diff)}\n\nEnd of diff.`
  if (kind === 'pr') return `Review the complete pull request diff (${text(input.label)}) at head ${text(input.head_sha)} and base ${text(input.base_sha)}. Binary changes are represented by metadata; their contents are not semantically inspected. Exact binary object IDs remain in the review evidence.\n\n${text(input.diff)}\n\nEnd of diff.`
  throw new Error('unadmitted review source')
}
function pass(input: Row): string {
  const manifest = row(input.manifest), part = row(input.pass), view = row(input.view)
  const task = list(manifest.skipped_files).length === 0
    ? 'Review only defects introduced in this pass. Use supplementary source to check surrounding guards and declarations; it does not expand the commentable diff. Binary contents and omitted callers are not inspected. No build or tests have been run.'
    : `Review only defects introduced in this pass. This is a partial review (pass ${integer(part.number)} of ${list(manifest.passes).length}): the gate did not read ${skipped(manifest.skipped_files)}. Do not claim full coverage. Use supplementary source to check surrounding guards and declarations; it does not expand the commentable diff. Binary contents and omitted callers are not inspected. No build or tests have been run.`
  if (!isJson(input.context)) throw new Error('invalid Core review context')
  const prompt = {task, untrusted_repository_data: true, pull_request: {number: integer(input.number), title: text(view.title), description: text(view.body)}, manifest, pass: part, diff: text(input.diff), repository_context: input.context,
    context_limit: 'Context is bounded supplementary excerpts from the exact head. Null means no source context could fit. Missing files or omitted lines are not evidence of a defect.'}
  return JSON.stringify(bool(input.sort_keys) ? ordered(prompt) : prompt)
}
function interactive(input: Row): string {
  const view = row(input.view), number = integer(input.number)
  const base = text(view.base), head = text(view.head)
  const branches = base && head ? `${base} ← ${head}` : base || head || '(unknown)'
  return `Review PR #${number} — ${trim(text(view.title)) || `(PR #${number})`}\n\nURL: ${text(view.url) || '(unavailable)'}\nBranches: ${branches}\nRevision: ${text(view.head_sha)} (base ${text(view.base_sha)}); ${integer(view.changed_files)} file patches.\nBinary changes are represented by metadata; their contents are not semantically inspected. Exact binary object IDs remain in the review evidence.\n\n## Description\n\n${trim(text(view.body)) || '(no description)'}\n\n## Diff\n\n\`\`\`diff\n${text(input.diff)}\n\`\`\`\n`
}
function fence(value: string): string { let longest = 0, run = 0; for (const char of value) { run = char === '`' ? run + 1 : 0; longest = Math.max(longest, run) } return '`'.repeat(Math.max(3, longest + 1)) }
function location(value: Row): string {
  const path = optional(value.path)
  return path === undefined ? '' : value.line === null || value.line === undefined ? `\`${trim(path)}\`` : `\`${trim(path)}:${integer(value.line)}\``
}
function report(input: Row): string {
  if (input.review === null) return text(input.output)
  const review = row(input.review), posted = bool(input.posted)
  let out = '## Codewhale review\n\n'
  const summary = text(review.summary), assessment = text(review.overall_assessment)
  if (summary.length) out += trim(summary) + '\n\n'
  const issues = list(review.issues), suggestions = list(review.suggestions)
  if (issues.length) {
    out += '### Findings\n\n'
    for (const item of issues) {
      const issue = row(item), at = location(issue)
      out += `- **[${text(issue.severity).toUpperCase()}] ${text(issue.title)}**${at ? ` (${at})` : ''}\n`
      const description = text(issue.description)
      if (description.length) out += `  ${description}\n`
    }
    out += '\n'
  }
  if (suggestions.length) {
    out += '### Suggestions\n\n'
    for (const item of suggestions) {
      const suggestion = row(item), at = location(suggestion)
      out += `- ${at ? `${at} — ` : ''}${text(suggestion.suggestion)}\n`
      const replacement = optional(suggestion.replacement)
      if (replacement !== undefined && trim(replacement).length) {
        const codeFence = fence(replacement)
        out += `\n  ${codeFence}${posted ? 'diff' : 'suggestion'}\n`
        for (const line of replacement.split('\n')) out += `  ${line}\n`
        out += `  ${codeFence}\n`
      }
    }
    out += '\n'
  }
  if (assessment.length) out += '### Assessment\n\n' + trim(assessment) + '\n\n'
  const tally = {blocker: 0, concern: 0, nit: 0, introduced: 0, preexisting: 0}
  for (const item of issues) {
    const issue = row(item), severity = text(issue.severity)
    if (severity === 'blocker') tally.blocker += 1
    else if (severity === 'concern') tally.concern += 1
    else if (severity === 'nit') tally.nit += 1
    if (optional(issue.authorship) === 'preexisting') tally.preexisting += 1
    else tally.introduced += 1
  }
  out += `<!-- codewhale-review: {"blocker":${tally.blocker},"concern":${tally.concern},"nit":${tally.nit},"introduced":${tally.introduced},"preexisting":${tally.preexisting}} -->\n`
  return out
}
export function transformReviewSnapshot(operation: string, value: unknown): Result {
  if (!isReviewOperation(operation)) throw new Error('unadmitted review operation')
  const input = row(value)
  const content = operation === 'review_source_prompt' ? source(input) : operation === 'review_pass_prompt' ? pass(input) : operation === 'review_interactive_pr' ? interactive(input) : report(input)
  const result: Result = {ok: true, result: {content, success: true, metadata: null}}
  // Same upper-bound JSON-RPC envelope as Core; limits include escaping and headers.
  if (reviewEnvelopeBytes(result) > REVIEW_LIMIT) throw new Error('serialized review result exceeds 16 MiB')
  return result
}
