import { appendFileSync, lstatSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { fingerprint, hasRecordedEvidence, issueBody, parseArtifact, renderSummary, repositoryPath } from './findings.mjs';
import { authorizeRequest, target, validateSnapshot } from './review.mjs';

function stop(outcome, reason) {
  throw Object.assign(new Error(reason), { outcome, reason });
}

export function rightLines(patch) {
  const ranges = [];
  let oldRemaining = 0;
  let newRemaining = 0;
  let line = 0;
  let hasHunk = false;
  for (const entry of (patch || '').split('\n')) {
    const hunk = /^@@ -\d+(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(entry);
    if (hunk) {
      if (oldRemaining || newRemaining) return [];
      oldRemaining = Number(hunk[1] ?? 1);
      line = Number(hunk[2]);
      newRemaining = Number(hunk[3] ?? 1);
      if (![oldRemaining, line, newRemaining].every(Number.isSafeInteger)) return [];
      hasHunk = true;
      continue;
    }
    if (!hasHunk) continue;
    if (entry.startsWith('\\')) continue;
    if (!oldRemaining && !newRemaining && entry === '') continue;
    if (entry.startsWith('+') && newRemaining > 0) {
      ranges.push(line++);
      newRemaining--;
    } else if (entry.startsWith('-') && oldRemaining > 0) {
      oldRemaining--;
    } else if (entry.startsWith(' ') && oldRemaining > 0 && newRemaining > 0) {
      ranges.push(line++);
      oldRemaining--;
      newRemaining--;
    } else return [];
  }
  return oldRemaining || newRemaining ? [] : ranges;
}

export function planPublication(artifact, files, reviews, reviewer) {
  const marker = `<!-- codewhale-action-review:v1:${fingerprint([artifact.repository, artifact.pr_number,
    artifact.base_sha, artifact.head_sha, artifact.review])} -->`;
  const authored = reviews.filter(item => item.user?.login === reviewer && item.state === 'COMMENTED');
  if (authored.some(item => item.body?.includes(marker))) return null;
  const known = new Set(authored.flatMap(item => [...(item.body || '').matchAll(/<!-- codewhale-action-finding:v1:([a-f0-9]{64}) -->/g)].map(match => match[1])));
  const byPath = new Map(files.map(file => [file.filename, file]));
  const normalized = value => value.trim().replace(/\s+/g, ' ').toLowerCase();
  const selected = [];
  const eligible = artifact.review.issues.filter(hasRecordedEvidence);
  for (const issue of eligible) {
    const file = byPath.get(issue.path);
    const id = fingerprint([artifact.repository, artifact.pr_number, issue.path,
      file?.sha || artifact.head_sha, issue.line, issue.severity, issue.authorship,
      normalized(issue.title), normalized(issue.description)]);
    if (!known.has(id)) selected.push({ issue, id, file });
    known.add(id);
  }
  if (eligible.length && selected.length === 0) return null;
  const body = [renderSummary(artifact, selected.map(item => item.issue), eligible.length - selected.length,
    artifact.review.issues.length - eligible.length),
    marker, ...selected.map(item => `<!-- codewhale-action-finding:v1:${item.id} -->`)].join('\n\n');
  const comments = selected.filter(({ issue, file }) => file && issue.line && issue.inline_anchorable !== false
    && issue.authorship === 'introduced'
    && rightLines(file.patch).includes(issue.line)).slice(0, 50)
    .map(({ issue }) => ({ path: issue.path, line: issue.line, side: 'RIGHT', body: issueBody(issue) }));
  if (Buffer.byteLength(body) > 60000 || comments.some(item => Buffer.byteLength(item.body) > 60000)) {
    stop('failed', 'review_exceeds_github_limit');
  }
  return { commit_id: artifact.head_sha, event: 'COMMENT', body, comments };
}

function command(program, args, options) {
  return spawnSync(program, args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024,
    timeout: 120000, killSignal: 'SIGKILL', ...options });
}

export function publish(env = process.env, execute = command) {
  const root = mkdtempSync(join(env.RUNNER_TEMP || tmpdir(), 'codewhale-publish-'));
  const receiptPath = join(root, 'publication.json');
  let receipt = { schema_version: 1, outcome: 'failed', reason: 'setup_failed', publication: 'not_attempted' };
  try {
    const event = JSON.parse(readFileSync(env.GITHUB_EVENT_PATH, 'utf8'));
    const { repo, number } = target(env, event);
    if (!env.GH_TOKEN) stop('configuration_missing', 'github_token_required');
    const reviewer = env.CW_REVIEWER_LOGIN || 'github-actions[bot]';
    if (!/^[a-zA-Z0-9][a-zA-Z0-9-]{0,99}(?:\[bot\])?$/.test(reviewer)) stop('configuration_missing', 'invalid_reviewer_login');
    const file = lstatSync(env.CW_FINDINGS || '');
    if (!file.isFile() || file.size > 2 * 1024 * 1024) stop('configuration_missing', 'invalid_findings_artifact');
    let artifact;
    try { artifact = parseArtifact(readFileSync(env.CW_FINDINGS, 'utf8')); }
    catch { stop('configuration_missing', 'invalid_findings_artifact'); }
    if (artifact.repository !== repo || artifact.pr_number !== number
      || artifact.run_id !== env.GITHUB_RUN_ID || artifact.run_attempt !== env.GITHUB_RUN_ATTEMPT) {
      stop('configuration_missing', 'artifact_context_mismatch');
    }
    const eligible = artifact.review.issues.filter(hasRecordedEvidence).length;
    receipt = { ...receipt, repository: repo, pr_number: number, head_sha: artifact.head_sha,
      base_sha: artifact.base_sha, findings: eligible,
      withheld_findings: artifact.review.issues.length - eligible };
    const childEnv = Object.fromEntries(['PATH', 'SystemRoot', 'TMPDIR', 'LANG'].filter(key => env[key]).map(key => [key, env[key]]));
    Object.assign(childEnv, { HOME: root, GH_CONFIG_DIR: join(root, 'gh'), GH_TOKEN: env.GH_TOKEN,
      GH_HOST: 'github.com', GH_PROMPT_DISABLED: '1', NO_COLOR: '1', CI: 'true' });
    const api = (path, paginate = false, input) => {
      const args = ['api', path, ...(paginate ? ['--paginate', '--slurp'] : []),
        ...(input ? ['--method', 'POST', '--input', '-'] : [])];
      const result = execute('gh', args, { cwd: root, env: childEnv, ...(input ? { input: JSON.stringify(input) } : {}) });
      if (result.status !== 0 || result.error) stop(input ? 'publication_uncertain' : 'failed',
        input ? 'inspect_github_before_retry' : 'github_read_failed');
      let data;
      try { data = JSON.parse(result.stdout); }
      catch { stop(input ? 'publication_uncertain' : 'failed', 'invalid_github_response'); }
      if (paginate) {
        if (!Array.isArray(data) || data.some(page => !Array.isArray(page))) stop('failed', 'invalid_github_response');
        return data.flat();
      }
      return data;
    };
    const snapshot = () => {
      const pr = api(`repos/${repo}/pulls/${number}`);
      validateSnapshot(pr, repo, event, env);
      if (pr.head.sha !== artifact.head_sha || pr.base.sha !== artifact.base_sha) stop('superseded', 'revision_changed');
      return pr;
    };
    const pr = snapshot();
    authorizeRequest(env, event, repo, number, api);
    const files = api(`repos/${repo}/pulls/${number}/files?per_page=100`, true);
    if (!Number.isSafeInteger(pr.changed_files) || files.length !== pr.changed_files
      || new Set(files.map(item => item.filename)).size !== files.length
      || files.some(item => !repositoryPath(item.filename) || !/^[a-f0-9]{40}(?:[a-f0-9]{24})?$/i.test(item.sha || '')
        || (item.patch != null && (typeof item.patch !== 'string' || Buffer.byteLength(item.patch) > 8 * 1024 * 1024)))) {
      stop('failed', 'incomplete_github_file_list');
    }
    const reviews = api(`repos/${repo}/pulls/${number}/reviews?per_page=100`, true);
    const plan = planPublication(artifact, files, reviews, reviewer);
    snapshot();
    authorizeRequest(env, event, repo, number, api);
    if (!plan) receipt = { ...receipt, outcome: 'already_published', reason: '', publication: 'deduplicated' };
    else {
      const posted = api(`repos/${repo}/pulls/${number}/reviews`, false, plan);
      if (!Number.isSafeInteger(posted.id) || posted.id <= 0 || posted.commit_id !== artifact.head_sha
        || posted.state !== 'COMMENTED' || posted.user?.login !== reviewer) {
        stop('publication_uncertain', 'inspect_github_before_retry');
      }
      receipt = { ...receipt, outcome: 'published', reason: '', publication: 'posted', review_id: posted.id,
        inline_comments: plan.comments.length };
    }
  } catch (error) {
    receipt = { ...receipt, outcome: error.outcome || 'failed', reason: error.reason || 'setup_failed',
      ...(error.outcome === 'publication_uncertain' ? { publication: 'uncertain' } : {}) };
  }
  writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`, { mode: 0o600 });
  if (env.GITHUB_OUTPUT) appendFileSync(env.GITHUB_OUTPUT, `outcome=${receipt.outcome}\nreceipt=${receiptPath}\n`);
  if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY,
    `### Codewhale publication\n\nOutcome: **${receipt.outcome}**. ${receipt.reason ? `Reason: \`${receipt.reason}\`.` : ''}\n`);
  const ok = ['published', 'already_published', 'superseded', 'not_eligible'].includes(receipt.outcome);
  console.log(`${ok ? '::notice::' : '::error::'}Codewhale publication: ${receipt.outcome} (${receipt.reason || 'complete'}).`);
  return { exitCode: ok ? 0 : 1, receipt, receiptPath };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = publish().exitCode;
