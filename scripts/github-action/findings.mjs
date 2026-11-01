import { createHash } from 'node:crypto';
import { KEYS } from './setup.mjs';

const SHA = /^[a-f0-9]{40}(?:[a-f0-9]{24})?$/i;
const REPO = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const MAX_BYTES = 2 * 1024 * 1024;

function requireValue(valid) {
  if (!valid) throw new Error('invalid_findings_artifact');
}

function text(value, max = 20000) {
  requireValue(typeof value === 'string' && value.length <= max && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(value));
  return value;
}

export function repositoryPath(value) {
  return typeof value === 'string' && value.length > 0 && value.length <= 4096
    && !/[\\\u0000-\u001f\u007f]/.test(value) && !value.startsWith('/')
    && !value.split('/').some(part => !part || part === '.' || part === '..');
}

function location(value) {
  requireValue(value.path == null || repositoryPath(value.path));
  requireValue(value.line == null || (Number.isSafeInteger(value.line) && value.line > 0));
  return { path: value.path ?? null, line: value.line ?? null };
}

function list(value, convert, max = 200) {
  requireValue(Array.isArray(value) && value.length <= max);
  return value.map(convert);
}

function review(value) {
  requireValue(value && typeof value === 'object');
  const result = {
    summary: text(value.summary),
    issues: list(value.issues, issue => {
      requireValue(issue && ['blocker', 'concern', 'nit', 'error', 'warning', 'info', 'high', 'medium', 'low', 'critical'].includes(issue.severity));
      requireValue(issue.authorship == null || ['introduced', 'preexisting'].includes(issue.authorship));
      requireValue(issue.inline_anchorable == null || typeof issue.inline_anchorable === 'boolean');
      const item = { ...location(issue), severity: issue.severity, title: text(issue.title, 1000),
        description: text(issue.description), authorship: issue.authorship ?? null,
        inline_anchorable: issue.inline_anchorable ?? null };
      if (issue.evidence != null) {
        requireValue(['reproduced', 'traced', 'suspected'].includes(issue.evidence.tier));
        item.evidence = { tier: issue.evidence.tier, detail: text(issue.evidence.detail) };
      }
      if (issue.contract != null) item.contract = { became: text(issue.contract.became),
        dependents_checked: list(issue.contract.dependents_checked, item => text(item, 4096), 100) };
      if (issue.kind != null) {
        requireValue(['defect', 'intent', 'contract'].includes(issue.kind));
        item.kind = issue.kind;
      }
      return item;
    }),
    suggestions: list(value.suggestions, item => ({ ...location(item), suggestion: text(item.suggestion) })),
    overall_assessment: text(value.overall_assessment),
    verified: list(value.verified ?? [], item => text(item), 100),
    not_reviewed: list(value.not_reviewed ?? [], item => text(item), 100),
    ruled_out: list(value.ruled_out ?? [], item => text(item), 100),
  };
  if (value.merge_risk != null) {
    requireValue(['low', 'medium', 'high'].includes(value.merge_risk.level));
    result.merge_risk = { level: value.merge_risk.level, reason: text(value.merge_risk.reason) };
  }
  return result;
}

export function validateArtifact(value) {
  requireValue(value?.schema_version === 1 && value.kind === 'codewhale-review-findings');
  requireValue(REPO.test(value.repository || '') && Number.isSafeInteger(value.pr_number) && value.pr_number > 0);
  requireValue(SHA.test(value.base_sha || '') && SHA.test(value.head_sha || ''));
  requireValue(/^[1-9][0-9]*$/.test(value.run_id || '') && /^[1-9][0-9]*$/.test(value.run_attempt || ''));
  requireValue(value.complete === true && value.publication === 'not_requested');
  requireValue(Number.isSafeInteger(value.review_passes) && value.review_passes > 0 && value.review_passes <= 64);
  const result = { schema_version: 1, kind: value.kind, repository: value.repository, pr_number: value.pr_number,
    base_sha: value.base_sha, head_sha: value.head_sha, run_id: value.run_id, run_attempt: value.run_attempt,
    complete: true, publication: 'not_requested', review_passes: value.review_passes, review: review(value.review) };
  requireValue(Buffer.byteLength(JSON.stringify(result)) <= MAX_BYTES);
  return result;
}

export function parseArtifact(raw) {
  requireValue(Buffer.byteLength(raw) <= MAX_BYTES);
  return validateArtifact(JSON.parse(raw));
}

export function findingsArtifact(data, receipt, env) {
  const secrets = ['GH_TOKEN', ...Object.values(KEYS)].map(key => env[key]).filter(Boolean);
  const clean = value => typeof value === 'string'
    ? secrets.reduce((result, secret) => result.split(secret).join('[REDACTED]'), value)
    : Array.isArray(value) ? value.map(clean)
      : value && typeof value === 'object' ? Object.fromEntries(Object.entries(value).map(([key, item]) => [key, clean(item)])) : value;
  return validateArtifact({ schema_version: 1, kind: 'codewhale-review-findings', repository: receipt.repository,
    pr_number: receipt.pr_number, base_sha: receipt.base_sha, head_sha: receipt.head_sha,
    run_id: env.GITHUB_RUN_ID, run_attempt: env.GITHUB_RUN_ATTEMPT, complete: true,
    publication: 'not_requested', review_passes: data.review_passes, review: clean(review(data.review)) });
}

export function fingerprint(value) {
  return createHash('sha256').update(JSON.stringify(value)).digest('hex');
}

export function markdown(value) {
  return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('@', '&#64;').replace(/[\\`*_{}\[\]()!|#~]/g, '\\$&');
}

export function issueBody(issue) {
  let body = `**[${issue.severity.toUpperCase()}] ${markdown(issue.title)}**\n\n${markdown(issue.description)}`;
  if (issue.evidence) body += `\n\n${issue.evidence.tier}: ${markdown(issue.evidence.detail)}`;
  if (issue.contract) body += `\n\nContract: ${markdown(issue.contract.became)}\n\nDependents checked: ${issue.contract.dependents_checked.map(markdown).join(', ')}`;
  return body;
}

export function hasRecordedEvidence(issue) {
  return ['reproduced', 'traced'].includes(issue.evidence?.tier)
    && typeof issue.evidence.detail === 'string' && issue.evidence.detail.trim().length > 0;
}

export function renderSummary(value, issues, duplicateCount, withheldCount = 0) {
  const result = ['## Codewhale review', `Reviewed commit: ${value.head_sha}`,
    issues.length ? `${issues.length} finding(s) with recorded evidence.`
      : 'No new findings met the publication evidence requirement.'];
  for (const issue of issues.filter(item => item.authorship !== 'preexisting')) {
    const location = issue.path ? `\n\nLocation: ${markdown(issue.path)}${issue.line ? `:${issue.line}` : ''}` : '';
    result.push(`${issue.authorship == null ? 'Attribution to this change was not recorded.\n\n' : ''}${issueBody(issue)}${location}`);
  }
  const preexisting = issues.filter(item => item.authorship === 'preexisting');
  if (preexisting.length) result.push('<details><summary>Pre-existing findings</summary>\n\n'
    + preexisting.map(issue => `${issueBody(issue)}${issue.path ? `\n\nLocation: ${markdown(issue.path)}${issue.line ? `:${issue.line}` : ''}` : ''}`).join('\n\n')
    + '\n\n</details>');
  if (duplicateCount) result.push(`${duplicateCount} finding(s) already reported by this publisher on unchanged file content were omitted.`);
  if (withheldCount) result.push(`${withheldCount} candidate finding(s) had missing, empty or suspected evidence and were withheld. Investigation details remain in the analysis artifact.`);
  result.push(`[Analysis and coverage](https://github.com/${value.repository}/actions/runs/${value.run_id}). Recorded evidence is supplied by the reviewer; publication does not independently reproduce it.`,
    'Advisory review, not an approval or proof that the change is bug-free. Binary contents and arbitrary unchanged callers are outside its coverage.');
  return result.filter(Boolean).join('\n\n');
}
