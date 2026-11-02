/** Pure presentation of admitted Core GitHub outcomes. No network, process, store or policy API. */
import type { Json } from '../../protocol.ts'
interface Result { ok: true; result: { content: string; success: boolean; metadata: Json } }
type Row = Record<string, unknown>
const REPOSITORY = 'codewhale-hq/CodeWhale'
function row(value: unknown): Row { if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('invalid GitHub snapshot'); return value as Row }
function text(value: unknown): string { if (typeof value !== 'string') throw new Error('invalid GitHub text'); return value }
function strings(value: unknown): string[] { if (!Array.isArray(value)) throw new Error('invalid GitHub list'); return value.map(text) }
// Rust char indices count Unicode scalars before discarding control characters.
export function githubSummary(value: string, limit: number): string {
  let out = '', index = 0
  for (const char of value) {
    if (index++ >= Math.max(0, limit - 3)) return `${out}...`
    const code = char.codePointAt(0)!
    if ((code <= 31 || (code >= 127 && code <= 159)) && char !== '\n' && char !== '\t') continue
    out += char
  }
  return out
}
export function renderIssueReview(value: unknown): string {
  const report = row(value), fields = row(report.fields), id = text(report.id)
  let out = `# ${text(fields.title)}\n\nDraft: ${id}\nStatus: ready for review\nPublication: unavailable\nDuplicate search: not performed\nDestination: ${REPOSITORY}\n\nReview the contents before sharing. Redaction does not guarantee privacy.\n`
  if (report.revises !== null) out += `Revises: ${text(report.revises)}\n`
  for (const [heading, key] of [['Expected behavior','expected'],['Actual behavior','actual'],['Impact','impact']]) out += `\n## ${heading}\n\n${text(fields[key])}\n`
  for (const [heading, key] of [['Steps to reproduce (agent reported)','steps'],['Observed by the agent','observed'],['Inferences (not verified)','inferred']]) {
    const items = strings(fields[key]); out += `\n## ${heading}\n\n`
    out += items.length ? items.map(item => `- ${item}\n`).join('') : 'None recorded.\n'
  }
  out += `\n## Runtime context\n\n- Codewhale: ${text(report.version)}\n- Platform: ${text(report.platform)}\n- Active model: ${text(report.model)}\n`
  for (const [label,key] of [['Provider','reported_provider'],['Tool','reported_tool'],['Terminal','reported_terminal']]) out += `- ${label} (agent reported): ${fields[key] === null ? 'unknown' : text(fields[key])}\n`
  if (!Array.isArray(fields.related_issues) || fields.related_issues.some(value => !Number.isInteger(value) || Number(value) < 1 || Number(value) > 4294967295)) throw new Error('invalid related issue')
  if (fields.related_issues.length) out += '\n## Related issues (agent supplied; not verified or searched)\n\n' + fields.related_issues.map(number => `- [#${number}](https://github.com/${REPOSITORY}/issues/${number})\n`).join('')
  const redactions = strings(report.redactions)
  if (redactions.length) out += `\nRedacted categories: ${redactions.join(', ')}\n`
  return out + `\nReview: \`/feedback review ${id}\`\nRevise: \`/feedback edit ${id} <change>\`\n`
}
export function transformGithubSnapshot(value: unknown): Result {
  const snapshot = row(value), action = text(snapshot.action)
  if (action === 'report_draft' || action === 'report_read' || action === 'report_review') {
    const report = row(snapshot.report), id = text(report.id)
    const review = renderIssueReview(report)
    const content = action === 'report_review' ? review : JSON.stringify({ report_id:id, revises:report.revises, state:'ready_for_review', publication:'unavailable', duplicate_search:'not_performed', review, artifact:`artifacts/issue-reports/${id}.json` })
    return { ok:true, result:{success:true,content,metadata:null} }
  }
  const number = text(snapshot.number)
  if (!/^[0-9]{1,20}$/.test(number)) throw new Error('invalid captured issue number')
  if (action === 'issue_context' || action === 'pr_context') {
    const raw = structuredClone(row(snapshot.raw)), kind = action === 'issue_context' ? 'issue' : 'pr'
    if (snapshot.large_body === true) {
      const body = text(raw.body)
      raw.body_summary = githubSummary(body,900); raw.body_artifact = snapshot.body_artifact as Json; raw.body = githubSummary(body,1200)
    }
    if (action === 'pr_context' && snapshot.diff !== null) { raw.diff_summary=githubSummary(text(snapshot.diff),900); raw.diff_artifact=snapshot.diff_artifact as Json }
    const subject = kind === 'issue' ? 'Issue' : 'PR'
    return { ok:true, result:{success:true,metadata:null,content:JSON.stringify({summary:`${subject} #${number}: ${typeof raw.title === 'string' ? raw.title : ''}`,[kind]:raw})} }
  }
  const target = text(snapshot.target)
  if (!['issue','pr'].includes(target)) throw new Error('invalid captured GitHub target')
  const subject = target === 'issue' ? 'issue' : 'PR'
  let content: string
  if (snapshot.dirty === true) content = `Refusing to close ${subject}: worktree is dirty and allow_dirty was false.`
  else if (action === 'comment') content = snapshot.dry_run === true ? `Dry run: would comment on ${target} #${number}.` : `Commented on ${target} #${number}.`
  else if (action === 'close_issue' || action === 'close_pr') content = snapshot.dry_run === true ? `Dry run: would close ${subject} #${number}.` : `Closed ${subject} #${number}.`
  else throw new Error('unknown captured GitHub operation')
  return {ok:true,result:{success:snapshot.dirty !== true,content,metadata:null}}
}
