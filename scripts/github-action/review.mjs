// GitHub orchestration only: the released CLI owns diff coverage, prompts,
// model routing, anchor validation and the final pre-publication head check.
// No repository checkout, project configuration, hooks, or PR code is executed.
// Known limits: review only, including explicit review mentions; no dollar-budget
// guarantee or automatic retry. Legacy direct publication has no cross-run deduplication.
import { appendFileSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { findingsArtifact } from './findings.mjs';
import { KEYS } from './setup.mjs';

const SHA = /^[a-f0-9]{40}(?:[a-f0-9]{24})?$/i;
const REPO = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const MODEL = /^[A-Za-z0-9][A-Za-z0-9_./:@+-]{0,199}$/;

class Stop extends Error {
  constructor(outcome, reason) { super(reason); this.outcome = outcome; this.reason = reason; }
}
const stop = (outcome, reason) => { throw new Stop(outcome, reason); };
function integer(value, fallback, min, max) {
  const text = String(value || fallback);
  if (!/^[1-9][0-9]*$/.test(text) || Number(text) < min || Number(text) > max) {
    stop('configuration_missing', 'invalid_limits');
  }
  return Number(text);
}

export function settings(env) {
  if (!/^v\d+\.\d+\.\d+$/.test(env.CW_VERSION || '')) stop('configuration_missing', 'exact_release_required');
  const provider = env.CW_PROVIDER || '';
  if (!provider) stop('configuration_missing', 'provider_required');
  const model = env.CW_MODEL || '';
  if (!MODEL.test(model)) stop('configuration_missing', 'exact_model_required');
  if (env.CODEWHALE_API_KEY && provider !== 'codewhale') stop('configuration_missing', 'account_key_requires_relay');
  if (provider === 'codewhale') {
    if (!env.CODEWHALE_API_KEY) stop('configuration_missing', 'account_key_required');
    if (!/^[A-Za-z0-9_.-]+\/.+/.test(model)) stop('configuration_missing', 'account_catalog_model_required');
  } else if (!Object.hasOwn(KEYS, provider) || !env[KEYS[provider]]) {
    stop('configuration_missing', 'provider_key_required');
  }
  if (!env.GH_TOKEN) stop('configuration_missing', 'github_token_required');
  if (!['true', 'false'].includes(env.CW_POST || 'true')) stop('configuration_missing', 'invalid_post');
  const reasoningEffort = env.CW_REASONING_EFFORT || 'auto';
  if (!['auto', 'off', 'low', 'medium', 'high', 'max'].includes(reasoningEffort)) stop('configuration_missing', 'invalid_reasoning_effort');
  return {
    version: env.CW_VERSION, provider, model, reasoningEffort, post: env.CW_POST !== 'false',
    maxChars: integer(env.CW_MAX_CHARS, 200000, 1, 8388608),
    maxPasses: integer(env.CW_MAX_PASSES, 1, 1, 64),
    timeout: integer(env.CW_TIMEOUT_SECONDS, 600, 30, 1200) * 1000,
    outputTokens: env.CW_MAX_OUTPUT_TOKENS ? integer(env.CW_MAX_OUTPUT_TOKENS, 8192, 8192, 1000000) : null,
  };
}

export function target(env, event) {
  if (env.GITHUB_SERVER_URL && env.GITHUB_SERVER_URL !== 'https://github.com') stop('configuration_missing', 'unsupported_github_host');
  if (!REPO.test(env.GITHUB_REPOSITORY || '')) stop('configuration_missing', 'invalid_repository');
  let number;
  if (env.GITHUB_EVENT_NAME === 'pull_request') {
    if (!['opened', 'synchronize', 'reopened', 'ready_for_review'].includes(event.action)) stop('not_eligible', 'unsupported_pr_action');
    if (event.pull_request?.draft) stop('not_eligible', 'draft');
    if (event.pull_request?.head?.repo?.full_name !== env.GITHUB_REPOSITORY) stop('not_eligible', 'fork');
    if (event.pull_request?.base?.repo?.full_name !== env.GITHUB_REPOSITORY) stop('configuration_missing', 'repository_mismatch');
    if (!SHA.test(event.pull_request?.head?.sha || '') || !SHA.test(event.pull_request?.base?.sha || '')) stop('configuration_missing', 'invalid_revision');
    number = event.number;
  } else if (env.GITHUB_EVENT_NAME === 'workflow_dispatch') {
    number = env.CW_PR_NUMBER;
  } else if (['issue_comment', 'pull_request_review_comment'].includes(env.GITHUB_EVENT_NAME)) {
    if (event.action !== 'created') stop('not_eligible', 'unsupported_comment_action');
    if (event.comment?.user?.type !== 'User' || event.sender?.type !== 'User'
      || event.comment.user.login !== env.GITHUB_ACTOR || event.sender.login !== env.GITHUB_ACTOR
      || !Number.isSafeInteger(event.comment.id) || event.comment.id < 1) stop('not_eligible', 'untrusted_actor');
    const trigger = env.CW_TRIGGER_PHRASE || '@codewhale';
    if (!/^@[A-Za-z0-9][A-Za-z0-9-]{0,38}$/.test(trigger)) stop('configuration_missing', 'invalid_trigger_phrase');
    if (typeof event.comment.body !== 'string'
      || event.comment.body.trim().toLowerCase() !== `${trigger.toLowerCase()} review`) stop('not_eligible', 'review_mention_required');
    if (env.GITHUB_EVENT_NAME === 'issue_comment') {
      if (!event.issue?.pull_request) stop('not_eligible', 'pull_request_required');
      number = event.issue.number;
    } else number = event.pull_request?.number;
  } else {
    // In particular, pull_request_target must not gain an inference path.
    stop('not_eligible', 'unsupported_event');
  }
  if (!/^[1-9][0-9]{0,9}$/.test(String(number || ''))) stop('configuration_missing', 'invalid_pr_number');
  return { repo: env.GITHUB_REPOSITORY, number: Number(number) };
}

export function validateSnapshot(pr, repo, event, env = {}) {
  if (pr?.base?.repo?.full_name !== repo) stop('configuration_missing', 'repository_mismatch');
  if (pr?.head?.repo?.full_name !== repo && !(env.CW_ALLOW_FORKS === 'true'
    && ['issue_comment', 'workflow_dispatch'].includes(env.GITHUB_EVENT_NAME))) stop('not_eligible', 'fork');
  if (pr.state !== 'open' || pr.draft) stop('not_eligible', 'closed_or_draft');
  if (!SHA.test(pr.head.sha) || !SHA.test(pr.base.sha)) stop('configuration_missing', 'invalid_revision');
  if (event.pull_request && (event.pull_request.head.sha !== pr.head.sha || event.pull_request.base.sha !== pr.base.sha)) {
    stop('superseded', 'revision_changed');
  }
}

export function authorizeRequest(env, event, repo, number, api) {
  if (!['true', 'false'].includes(env.CW_ALLOW_FORKS || 'false')) stop('configuration_missing', 'invalid_allow_forks');
  const mention = ['issue_comment', 'pull_request_review_comment'].includes(env.GITHUB_EVENT_NAME);
  if (!mention && !(env.GITHUB_EVENT_NAME === 'workflow_dispatch' && env.CW_ALLOW_FORKS === 'true')) return;
  const actor = env.GITHUB_ACTOR;
  const rerunner = env.GITHUB_TRIGGERING_ACTOR || actor;
  if (event.sender?.type !== 'User' || event.sender.login !== actor
    || ![actor, rerunner].every(login => /^[A-Za-z0-9][A-Za-z0-9-]{0,38}$/.test(login || ''))) stop('not_eligible', 'untrusted_actor');
  for (const login of new Set([actor, rerunner])) {
    const permission = api(`repos/${repo}/collaborators/${login}/permission`);
    if (permission.user?.login !== login || permission.user?.type !== 'User'
      || !['admin', 'maintain', 'write'].includes(permission.permission)) stop('not_eligible', 'write_access_required');
  }
  if (env.CW_ALLOW_FORKS === 'true') {
    const repository = api(`repos/${repo}`);
    if (repository.full_name !== repo || !repository.default_branch
      || env.GITHUB_REF !== `refs/heads/${repository.default_branch}`) stop('not_eligible', 'default_branch_required');
  }
  if (mention) {
    const kind = env.GITHUB_EVENT_NAME === 'issue_comment' ? 'issues' : 'pulls';
    const comment = api(`repos/${repo}/${kind}/comments/${event.comment.id}`);
    const url = env.GITHUB_EVENT_NAME === 'issue_comment' ? comment.issue_url : comment.pull_request_url;
    const expected = `https://api.github.com/repos/${repo}/${env.GITHUB_EVENT_NAME === 'issue_comment' ? 'issues' : 'pulls'}/${number}`;
    if (comment.id !== event.comment.id || comment.user?.login !== actor || comment.user?.type !== 'User'
      || comment.body !== event.comment.body || comment.updated_at !== event.comment.updated_at
      || url !== expected) stop('not_eligible', 'comment_changed_or_mismatched');
  }
}

// Do not persist raw stderr, prompts, findings, PR titles, provider messages,
// or arbitrary JSON fields: they can contain credentials or Actions commands.
export function classify(result, head, post, base) {
  let data;
  try { data = JSON.parse(result.stdout); } catch { /* fail closed below */ }
  const publication = ['not_requested', 'not_attempted', 'uncertain', 'posted'].includes(data?.publication)
    ? data.publication : 'unknown';
  const usage = {};
  for (const key of ['input_tokens', 'output_tokens', 'reasoning_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens']) {
    if (Number.isSafeInteger(data?.usage?.[key]) && data.usage[key] >= 0) usage[key] = data.usage[key];
  }
  const evidence = { publication, usage };
  const coverage = data?.receipt?.coverage;
  if (publication === 'uncertain' || (result.error && post)) {
    return { ...evidence, outcome: 'publication_uncertain', reason: 'inspect_github_before_retry' };
  }
  if (data?.success === true && data.complete === false) {
    return { ...evidence, outcome: 'incomplete', reason: 'coverage_incomplete' };
  }
  if (data?.mode === 'review' && data.success === false && data.complete === false
    && data.outcome === 'infrastructure' && data.review_verdict === 'not_produced') {
    return { ...evidence, outcome: 'incomplete', reason: 'review_not_produced' };
  }
  if (result.status !== 0 || data?.mode !== 'review' || data.success !== true || data.complete !== true
    || data.pr?.head_sha !== head || !Array.isArray(data.review?.issues)
    || !Number.isSafeInteger(data.review_passes) || data.review_passes < 1
    || coverage?.manifest?.head_sha !== head || coverage?.manifest?.base_sha !== base
    || !Array.isArray(coverage?.manifest?.skipped_files) || coverage.manifest.skipped_files.length !== 0
    || coverage?.completed_passes?.length !== data.review_passes
    || publication !== (post ? 'posted' : 'not_requested')) {
    return { ...evidence, outcome: 'failed', reason: result.error ? 'process_failed' : 'review_not_completed' };
  }
  return { ...evidence, outcome: data.review.issues.length ? 'reviewed_with_findings' : 'reviewed_clean',
    reason: '', findings: data.review.issues.length, passes: data.review_passes };
}

function command(program, args, options) {
  return spawnSync(program, args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024,
    timeout: 120000, killSignal: 'SIGKILL', ...options });
}

export function run(env = process.env, execute = command) {
  const root = mkdtempSync(join(env.RUNNER_TEMP || tmpdir(), 'codewhale-review-'));
  const receiptPath = join(root, 'receipt.json');
  let findingsPath = '';
  let receipt = { schema_version: 1, outcome: 'failed', reason: 'setup_failed', publication: 'not_attempted' };
  let prUrl = '';
  try {
    const event = JSON.parse(readFileSync(env.GITHUB_EVENT_PATH, 'utf8'));
    const { repo, number } = target(env, event);
    const config = settings(env);
    if (config.post && (['issue_comment', 'pull_request_review_comment'].includes(env.GITHUB_EVENT_NAME)
      || env.CW_ALLOW_FORKS === 'true')) stop('configuration_missing', 'separate_publisher_required');
    receipt = { ...receipt, repository: repo, pr_number: number, version: config.version,
      provider: config.provider, model: config.model, reasoning_effort: config.reasoningEffort, limits: { max_chars: config.maxChars,
        max_passes: config.maxPasses, timeout_seconds: config.timeout / 1000, max_output_tokens: config.outputTokens } };
    // Isolate user/project configuration and retain only the selected model key.
    // Never execute with unrelated runner credentials or Git configuration.
    const childEnv = Object.fromEntries(['PATH', 'SystemRoot', 'TMPDIR', 'LANG'].filter(k => env[k]).map(k => [k, env[k]]));
    Object.assign(childEnv, { HOME: join(root, 'home'), CODEWHALE_HOME: join(root, 'home', '.codewhale'),
      GH_TOKEN: env.GH_TOKEN, GH_HOST: 'github.com', GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null',
      GIT_TERMINAL_PROMPT: '0', GIT_LFS_SKIP_SMUDGE: '1', NO_COLOR: '1', CI: 'true' });
    childEnv[config.provider === 'codewhale' ? 'CODEWHALE_API_KEY' : KEYS[config.provider]] =
      env[config.provider === 'codewhale' ? 'CODEWHALE_API_KEY' : KEYS[config.provider]];
    if (config.outputTokens) childEnv.CODEWHALE_MAX_OUTPUT_TOKENS = String(config.outputTokens);
    mkdirSync(childEnv.CODEWHALE_HOME, { recursive: true, mode: 0o700 });
    if (config.reasoningEffort !== 'auto') {
      childEnv.CODEWHALE_CONFIG_PATH = join(root, 'review.toml');
      writeFileSync(childEnv.CODEWHALE_CONFIG_PATH, `reasoning_effort = "${config.reasoningEffort}"\n`, { mode: 0o600 });
    }
    const checked = (program, args, reason, extra = {}) => {
      const result = execute(program, args, { env: childEnv, cwd: root, ...extra });
      if (result.status !== 0 || result.error) stop('failed', reason);
      return result.stdout.trim();
    };
    let pr;
    try { pr = JSON.parse(checked('gh', ['api', `repos/${repo}/pulls/${number}`], 'github_snapshot_unavailable')); }
    catch (error) { if (error instanceof Stop) throw error; stop('failed', 'invalid_github_response'); }
    validateSnapshot(pr, repo, event, env);
    authorizeRequest(env, event, repo, number, path => {
      try { return JSON.parse(checked('gh', ['api', path], 'github_authorization_unavailable')); }
      catch (error) { if (error instanceof Stop) throw error; stop('failed', 'invalid_github_response'); }
    });
    receipt.base_sha = pr.base.sha;
    receipt.head_sha = pr.head.sha;
    prUrl = `https://github.com/${repo}/pull/${number}`;
    // Reuse the public installer: exact release, checksums, atomic fresh install.
    const installEnv = { PATH: childEnv.PATH, HOME: childEnv.HOME, CODEWHALE_VERSION: config.version,
      CODEWHALE_INSTALL_DIR: join(root, 'bin') };
    checked('sh', [join(env.CW_ACTION_PATH, 'web/public/install.sh')], 'release_install_failed', { env: installEnv, timeout: 180000 });
    const cli = join(root, 'bin/codewhale');
    if (config.provider === 'codewhale') checked(cli, ['--no-project-config', 'account', 'agent'], 'account_preflight_failed', { timeout: 60000 });
    const repoDir = join(root, 'repository');
    checked('git', ['init', '--quiet', repoDir], 'git_init_failed');
    const git = args => checked('git', ['-C', repoDir, '-c', 'core.hooksPath=/dev/null', '-c', 'credential.helper=',
      '-c', 'credential.helper=!gh auth git-credential', ...args], 'git_snapshot_unavailable');
    git(['remote', 'add', 'origin', `https://github.com/${repo}.git`]);
    // No checkout: fetch objects, then point HEAD at the trusted base. Core
    // reads pinned blobs and diffs; filters, submodules and candidate code stay inert.
    git(['fetch', '--no-tags', '--no-recurse-submodules', 'origin', pr.base.sha, `+refs/pull/${number}/head:refs/codewhale-review/head`]);
    if (git(['rev-parse', 'refs/codewhale-review/head^{commit}']) !== pr.head.sha) stop('superseded', 'revision_changed');
    if (!SHA.test(git(['merge-base', '--all', pr.base.sha, pr.head.sha]))) stop('failed', 'merge_base_unavailable');
    git(['update-ref', 'HEAD', pr.base.sha]);
    const args = ['--no-project-config', '--provider', config.provider, '--model', config.model,
      'review', '--pr', String(number), '--repo', repo, '--max-chars', String(config.maxChars),
      '--max-passes', String(config.maxPasses), '--json', '--write-receipt', '--receipt-path', join(root, 'cli-receipt.json')];
    if (config.post) args.push('--post');
    const result = execute(cli, args, { cwd: repoDir, env: childEnv, timeout: config.timeout });
    receipt = { ...receipt, ...classify(result, pr.head.sha, config.post, pr.base.sha) };
    if (!config.post && ['reviewed_clean', 'reviewed_with_findings'].includes(receipt.outcome)) {
      const data = JSON.parse(result.stdout);
      if (data.pr?.number !== number || data.pr?.url !== prUrl) stop('failed', 'review_target_mismatch');
      let artifact;
      try { artifact = findingsArtifact(data, receipt, env); }
      catch { stop('failed', 'invalid_review_findings'); }
      const path = join(root, 'findings.json');
      writeFileSync(path, `${JSON.stringify(artifact, null, 2)}\n`, { mode: 0o600 });
      findingsPath = path;
    }
  } catch (error) {
    receipt = { ...receipt, outcome: error instanceof Stop ? error.outcome : 'failed',
      reason: error instanceof Stop ? error.reason : 'setup_failed' };
  }
  writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`, { mode: 0o600 });
  if (env.GITHUB_OUTPUT) appendFileSync(env.GITHUB_OUTPUT, `outcome=${receipt.outcome}\nreceipt=${receiptPath}\nfindings=${findingsPath}\npr-url=${prUrl}\n`);
  if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY,
    `### Codewhale review\n\nOutcome: **${receipt.outcome}**. ${receipt.reason ? `Reason: \`${receipt.reason}\`.` : ''}\n\n` +
    'Only `reviewed_clean` and `reviewed_with_findings` confirm complete model review. Findings are advisory.\n');
  const ok = ['reviewed_clean', 'reviewed_with_findings', 'not_eligible', 'superseded'].includes(receipt.outcome);
  // Constant vocabulary only; model/provider/repository text is never logged.
  console.log(`${ok ? '::notice::' : '::error::'}Codewhale review: ${receipt.outcome} (${receipt.reason || 'complete'}).`);
  return { exitCode: ok ? 0 : 1, receipt, receiptPath, findingsPath };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = run().exitCode;
