import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { classify, run, settings, target } from './review.mjs';

const HEAD = 'a'.repeat(40);
const BASE = 'b'.repeat(40);
const pr = { state: 'open', draft: false, base: { sha: BASE, repo: { full_name: 'owner/repo' } },
  head: { sha: HEAD, repo: { full_name: 'owner/repo' } } };
const event = { action: 'opened', number: 12, pull_request: pr };
const defaults = { GITHUB_REPOSITORY: 'owner/repo', GITHUB_EVENT_NAME: 'pull_request',
  CW_VERSION: 'v0.10.0', CW_PROVIDER: 'codewhale', CW_MODEL: 'provider/model',
  CW_ACTION_PATH: '/trusted/action', GH_TOKEN: 'github-secret', CODEWHALE_API_KEY: 'account-secret',
  GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1' };
const completion = (override = {}) => ({ mode: 'review', success: true, complete: true,
  pr: { head_sha: HEAD, number: 12, url: 'https://github.com/owner/repo/pull/12' }, review: { summary: 'No findings', issues: [], suggestions: [], overall_assessment: 'No findings' }, review_passes: 1, publication: 'posted',
  receipt: { coverage: { manifest: { head_sha: HEAD, base_sha: BASE, skipped_files: [] }, completed_passes: [{}] } },
  usage: { input_tokens: 10, output_tokens: 20 }, ...override });
const output = data => ({ status: 0, stdout: JSON.stringify(data), stderr: '' });

function fixture(t, { env = {}, payload = event, snapshot = pr, intercept } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'cw-action-test-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const input = { ...defaults, RUNNER_TEMP: dir, GITHUB_EVENT_PATH: join(dir, 'event.json'),
    GITHUB_OUTPUT: join(dir, 'output'), GITHUB_STEP_SUMMARY: join(dir, 'summary'), ...env };
  writeFileSync(input.GITHUB_EVENT_PATH, JSON.stringify(payload));
  const calls = [];
  const result = run(input, (program, args, opts) => {
    calls.push({ program, args, opts });
    const intercepted = intercept?.(program, args, opts);
    if (intercepted) return intercepted;
    if (program === 'gh') return output(snapshot);
    if (args.includes('rev-parse')) return { status: 0, stdout: `${HEAD}\n` };
    if (args.includes('merge-base')) return { status: 0, stdout: `${BASE}\n` };
    if (args.includes('review')) return output(completion());
    return { status: 0, stdout: '' };
  });
  return { ...result, calls, summary: readFileSync(input.GITHUB_STEP_SUMMARY, 'utf8'),
    saved: JSON.parse(readFileSync(result.receiptPath, 'utf8')), outputs: readFileSync(input.GITHUB_OUTPUT, 'utf8') };
}

test('a pinned complete review reuses the installer and CLI without executing PR source', t => {
  const result = fixture(t, { env: { DEEPSEEK_API_KEY: 'stale-vendor-secret', UNRELATED_TOKEN: 'private',
    CODEWHALE_RELEASE_BASE_URL: 'https://attacker.example', CW_MAX_OUTPUT_TOKENS: '8192' } });
  assert.equal(result.exitCode, 0);
  assert.equal(result.saved.outcome, 'reviewed_clean');
  assert.equal(result.saved.head_sha, HEAD);
  assert.deepEqual(result.saved.usage, { input_tokens: 10, output_tokens: 20 });
  assert.match(result.outputs, /pr-url=https:\/\/github.com\/owner\/repo\/pull\/12/);
  const install = result.calls.find(c => c.program === 'sh');
  assert.equal(install.args[0], '/trusted/action/web/public/install.sh');
  assert.equal(install.opts.env.CODEWHALE_VERSION, 'v0.10.0');
  assert.equal(install.opts.env.GH_TOKEN, undefined);
  assert.equal(install.opts.env.CODEWHALE_RELEASE_BASE_URL, undefined);
  const review = result.calls.find(c => c.args.includes('review'));
  assert.ok(review.args.includes('--json'));
  assert.ok(review.args.includes('--no-project-config'));
  assert.equal(review.opts.env.CODEWHALE_API_KEY, 'account-secret');
  assert.equal(review.opts.env.DEEPSEEK_API_KEY, undefined);
  assert.equal(review.opts.env.UNRELATED_TOKEN, undefined);
  assert.equal(review.opts.env.CODEWHALE_MAX_OUTPUT_TOKENS, '8192');
  assert.equal(result.calls.some(c => c.args.includes('checkout')), false);
  assert.equal(result.calls.some(c => c.program === 'cargo'), false);
  assert.doesNotMatch(JSON.stringify(result.saved) + result.summary, /account-secret|stale-vendor-secret/);
});

test('forks, draft PRs and pull_request_target make no network or model calls', t => {
  for (const options of [
    { payload: { ...event, pull_request: { ...pr, head: { ...pr.head, repo: { full_name: 'fork/repo' } } } } },
    { payload: { ...event, pull_request: { ...pr, draft: true } } },
    { env: { GITHUB_EVENT_NAME: 'pull_request_target' } },
  ]) {
    const result = fixture(t, options);
    assert.equal(result.saved.outcome, 'not_eligible');
    assert.equal(result.calls.length, 0);
  }
});

test('manual recovery validates the current PR and refuses forks', t => {
  const env = { GITHUB_EVENT_NAME: 'workflow_dispatch', CW_PR_NUMBER: '12' };
  assert.equal(fixture(t, { env, payload: {} }).saved.outcome, 'reviewed_clean');
  const result = fixture(t, { env, payload: {}, snapshot: { ...pr, head: { ...pr.head, repo: { full_name: 'fork/repo' } } } });
  assert.equal(result.saved.outcome, 'not_eligible');
  assert.equal(result.calls.length, 1);
});

test('stale events and a head changed during fetch stop before inference', t => {
  const stale = fixture(t, { snapshot: { ...pr, head: { ...pr.head, sha: 'c'.repeat(40) } } });
  assert.equal(stale.saved.outcome, 'superseded');
  assert.equal(stale.calls.length, 1);
  const moved = fixture(t, { intercept: (_p, args) => args.includes('rev-parse') && { status: 0, stdout: 'c'.repeat(40) } });
  assert.equal(moved.saved.outcome, 'superseded');
  assert.equal(moved.calls.some(c => c.args.includes('review')), false);
});

test('bad config cannot fall back to a stale vendor key or contact another GitHub host', t => {
  for (const env of [
    { CODEWHALE_API_KEY: '', DEEPSEEK_API_KEY: 'stale' }, { CW_PROVIDER: 'deepseek', DEEPSEEK_API_KEY: 'stale' },
    { CODEWHALE_API_KEY: '', CW_PROVIDER: 'orcarouter', CW_MODEL: 'orcarouter/free', DEEPSEEK_API_KEY: 'stale' },
    { CW_MODEL: 'guessed-model' }, { CW_MODEL: 'provider/model\n::error::injection' },
    { CW_VERSION: 'latest' }, { CW_MAX_PASSES: '65' }, { CW_MAX_CHARS: '8388609' },
    { CW_MAX_OUTPUT_TOKENS: '1000' }, { CW_TIMEOUT_SECONDS: '0' }, { CW_POST: 'maybe' },
    { GITHUB_SERVER_URL: 'https://github.enterprise.example' },
  ]) {
    const result = fixture(t, { env });
    assert.equal(result.saved.outcome, 'configuration_missing');
    assert.equal(result.exitCode, 1);
    assert.equal(result.calls.length, 0);
  }
  assert.equal(settings({ ...defaults, CODEWHALE_API_KEY: '', CW_PROVIDER: 'deepseek', CW_MODEL: 'exact-model', DEEPSEEK_API_KEY: 'vendor' }).provider, 'deepseek');
  const orca = fixture(t, { env: { CODEWHALE_API_KEY: '', CW_PROVIDER: 'orcarouter', CW_MODEL: 'orcarouter/free',
    ORCAROUTER_API_KEY: 'orca-fixture-secret', DEEPSEEK_API_KEY: 'stale' } });
  assert.equal(orca.exitCode, 0);
  const review = orca.calls.find(call => call.args.includes('review'));
  assert.equal(review.opts.env.ORCAROUTER_API_KEY, 'orca-fixture-secret');
  assert.equal(review.opts.env.DEEPSEEK_API_KEY, undefined);
  assert.equal(review.opts.env.CODEWHALE_API_KEY, undefined);
  assert.equal(review.args[review.args.indexOf('--model') + 1], 'orcarouter/free');
  assert.doesNotMatch(JSON.stringify(orca.saved) + orca.summary, /orca-fixture-secret/);
  assert.throws(() => target({ ...defaults, GITHUB_REPOSITORY: '../x' }, event));
});

test('release verification and account preflight failures stop before review and save safe receipts', t => {
  for (const fail of [(p) => p === 'sh', (_p, args) => args.includes('account')]) {
    const result = fixture(t, { intercept: (p, args) => fail(p, args) && { status: 1, stdout: 'account-secret', stderr: '::error::secret' } });
    assert.equal(result.exitCode, 1);
    assert.equal(result.saved.outcome, 'failed');
    assert.equal(result.calls.some(c => c.args.includes('review')), false);
    assert.doesNotMatch(JSON.stringify(result.saved) + result.summary, /account-secret|::error::secret/);
  }
});

test('zero exit, empty output, wrong head, incomplete coverage and malformed receipts never mean clean', () => {
  for (const result of [
    { status: 0, stdout: '' }, { status: 0, stdout: 'no problems' }, output({ success: true }),
    output(completion({ pr: { head_sha: BASE } })), output(completion({ review_passes: 0 })),
    output(completion({ publication: 'not_attempted' })), output(completion({ review: null })),
    output(completion({ receipt: null })),
    output(completion({ receipt: { coverage: { manifest: { head_sha: HEAD, base_sha: 'c'.repeat(40), skipped_files: [] }, completed_passes: [{}] } } })),
    { ...output(completion()), status: 1 },
    { status: 1, stdout: JSON.stringify({ success: false, error: 'HTTP 402: account-secret', publication: 'not_attempted' }) },
  ]) assert.equal(classify(result, HEAD, true, BASE).outcome, 'failed');
  assert.equal(classify(output(completion({ complete: false })), HEAD, true, BASE).outcome, 'incomplete');
  assert.equal(classify(output(completion({ publication: 'uncertain' })), HEAD, true, BASE).outcome, 'publication_uncertain');
  assert.equal(classify({ status: null, error: new Error('timeout') }, HEAD, true, BASE).outcome, 'publication_uncertain');
});

test('findings are advisory and preview has no publication claim', t => {
  const findings = classify(output(completion({ review: { issues: [{ description: 'private repository text' }] } })), HEAD, true, BASE);
  assert.equal(findings.outcome, 'reviewed_with_findings');
  assert.equal(findings.findings, 1);
  assert.doesNotMatch(JSON.stringify(findings), /private repository text/);
  const preview = fixture(t, { env: { CW_POST: 'false' }, intercept: (_p, args) => args.includes('review') && output(completion({ publication: 'not_requested' })) });
  assert.equal(preview.exitCode, 0);
  assert.equal(preview.calls.find(c => c.args.includes('review')).args.includes('--post'), false);
  const artifact = JSON.parse(readFileSync(preview.findingsPath, 'utf8'));
  assert.equal(artifact.publication, 'not_requested');
  assert.equal(artifact.head_sha, HEAD);
  assert.deepEqual(artifact.review.issues, []);
  assert.match(preview.outputs, /findings=.*findings\.json/);
});
