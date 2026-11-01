import { lstatSync, mkdirSync, realpathSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { parseArgs } from 'node:util';

export const KEYS = {
  deepseek: 'DEEPSEEK_API_KEY', anthropic: 'ANTHROPIC_API_KEY',
  openrouter: 'OPENROUTER_API_KEY', orcarouter: 'ORCAROUTER_API_KEY', zai: 'ZAI_API_KEY',
  'modelstudio-token-plan': 'MODELSTUDIO_API_KEY', codewhale: 'CODEWHALE_API_KEY',
};

const HELP = `Create a Codewhale review bot workflow in your repository.

  node setup.mjs --provider deepseek --model MODEL_ID --directory /path/to/repo

  --provider NAME       ${Object.keys(KEYS).join(', ')}
  --model ID            Exact model ID; account mode requires provider/model
  --reasoning-effort E  auto, off, low, medium, high, max (default: off)
  --directory PATH      Destination repository (default: current directory)
  --version vX.Y.Z      Checksummed CLI release (default: v0.10.2)
  --action-ref SHA      Reviewed Action commit (default: this clean source checkout)
  --trigger-phrase NAME Mention to listen for (default: @codewhale)
  --app-login LOGIN     Optional existing GitHub App login, including [bot]
  --max-output-tokens N Review output allowance (default: 16384; minimum: 8192)
  --print               Print YAML without writing files
  --help                Show this help

Creates .github/workflows/codewhale-review.yml without overwriting existing files.
Set the selected provider secret, then commit the workflow to the default branch.
Automatic reviews cover same-repository PRs. Maintainers can review fork PRs
with a manual run or an @codewhale review comment; PR code is never executed.
`;

export function workflow({ provider, model, version = 'v0.10.2', actionRef, triggerPhrase = '@codewhale', appLogin, maxOutputTokens = '16384', reasoningEffort = 'off' }) {
  if (!Object.hasOwn(KEYS, provider)) throw new Error('Choose a supported --provider.');
  if (!/^[A-Za-z0-9][A-Za-z0-9_./:@+-]{0,199}$/.test(model || '')
    || (provider === 'codewhale' && !/^[A-Za-z0-9_.-]+\/.+/.test(model))) throw new Error('Choose an exact --model ID.');
  if (!/^v\d+\.\d+\.\d+$/.test(version)) throw new Error('--version must be an exact vX.Y.Z release.');
  if (!/^[a-f0-9]{40}$/i.test(actionRef || '')) throw new Error('--action-ref must be a full reviewed commit SHA.');
  if (!/^@[A-Za-z0-9][A-Za-z0-9-]{0,38}$/.test(triggerPhrase)) throw new Error('--trigger-phrase must be @ followed by a GitHub-style name.');
  if (appLogin && !/^[a-zA-Z0-9][a-zA-Z0-9-]{0,99}\[bot\]$/.test(appLogin)) throw new Error('--app-login must include [bot].');
  if (!/^[1-9][0-9]*$/.test(String(maxOutputTokens)) || Number(maxOutputTokens) < 8192 || Number(maxOutputTokens) > 1000000) throw new Error('--max-output-tokens must be between 8192 and 1000000.');
  if (!['auto', 'off', 'low', 'medium', 'high', 'max'].includes(reasoningEffort)) throw new Error('Choose a supported --reasoning-effort.');
  const action = `codewhale-hq/Codewhale@${actionRef}`;
  const secret = KEYS[provider];
  const expression = value => '${{ ' + value + ' }}';
  const eligible = `      (github.event_name == 'workflow_dispatch') ||
      (github.event_name == 'pull_request' && !github.event.pull_request.draft && github.event.pull_request.head.repo.full_name == github.repository) ||
      ((github.event_name == 'issue_comment' || github.event_name == 'pull_request_review_comment') &&
       (github.event.issue.pull_request || github.event.pull_request) &&
       github.event.comment.user.type == 'User' && contains(github.event.comment.body, '${triggerPhrase}'))`;
  const appToken = `      - uses: actions/create-github-app-token@64ab729a6294000f5aa4a4bc0b73d1f32bc45510
        id: app-token
        with:
          app-id: ${expression('vars.CODEWHALE_APP_ID')}
          private-key: ${expression('secrets.CODEWHALE_APP_PRIVATE_KEY')}
          permission-contents: read
          permission-pull-requests: write
`;
  return `name: Codewhale PR Review
on:
  pull_request:
    types: [opened, synchronize, reopened, ready_for_review]
  issue_comment:
    types: [created]
  pull_request_review_comment:
    types: [created]
  workflow_dispatch:
    inputs:
      pr-number:
        description: Pull request number
        required: true
        type: string
permissions: {}
concurrency:
  group: codewhale-review-${expression('github.event.pull_request.number || github.event.issue.number || inputs.pr-number')}
  cancel-in-progress: false
jobs:
${appLogin ? `  installation:
    if: >-
${eligible}
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions: {}
    steps:
${appToken}` : ''}  review:
${appLogin ? '    needs: installation\n' : ''}    if: >-
${eligible}
    runs-on: ubuntu-latest
    timeout-minutes: 25
    permissions:
      contents: read
      pull-requests: read
    outputs:
      findings: ${expression('steps.review.outputs.findings')}
    steps:
      - uses: actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1
        with:
          node-version: '22'
      - uses: ${action}
        id: review
        with:
          version: '${version}'
          provider: '${provider}'
          model: '${model}'
          reasoning-effort: '${reasoningEffort}'
          max-output-tokens: '${maxOutputTokens}'
          pr-number: ${expression('inputs.pr-number')}
          trigger-phrase: '${triggerPhrase}'
          allow-forks: 'true'
          post: 'false'
        env:
          ${secret}: ${expression("(github.event_name == 'workflow_dispatch' || github.event_name == 'issue_comment' || github.event.pull_request.head.repo.full_name == github.repository) && secrets." + secret + " || ''")}
      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9
        if: steps.review.outputs.findings != ''
        with:
          name: codewhale-findings-${expression('github.run_id')}-${expression('github.run_attempt')}
          path: ${expression('steps.review.outputs.findings')}
          retention-days: 1
          if-no-files-found: error
      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9
        if: always() && steps.review.outputs.receipt != ''
        with:
          name: codewhale-review-receipt-${expression('github.run_id')}-${expression('github.run_attempt')}
          path: ${expression('steps.review.outputs.receipt')}
          retention-days: 14
          if-no-files-found: error
  publish:
    needs: review
    if: needs.review.outputs.findings != ''
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: read
      pull-requests: write
    steps:
      - uses: actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1
        with:
          node-version: '22'
      - uses: actions/download-artifact@37930b1c2abaa49bbe596cd826c3c89aef350131
        with:
          name: codewhale-findings-${expression('github.run_id')}-${expression('github.run_attempt')}
          path: findings
${appLogin ? appToken : ''}      - uses: codewhale-hq/Codewhale/scripts/github-action/publish@${actionRef}
        id: publish
        with:
          findings: findings/findings.json
          pr-number: ${expression('inputs.pr-number')}
          trigger-phrase: '${triggerPhrase}'
          allow-forks: 'true'
${appLogin ? `          github-token: ${expression('steps.app-token.outputs.token')}
          reviewer-login: '${appLogin}'
` : ''}      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9
        if: always() && steps.publish.outputs.receipt != ''
        with:
          name: codewhale-publication-receipt-${expression('github.run_id')}-${expression('github.run_attempt')}
          path: ${expression('steps.publish.outputs.receipt')}
          retention-days: 14
          if-no-files-found: error
`;
}

export function setup(args = process.argv.slice(2)) {
  const { values } = parseArgs({ args, options: {
    provider: { type: 'string' }, model: { type: 'string' }, directory: { type: 'string' },
    version: { type: 'string' }, 'action-ref': { type: 'string' }, 'trigger-phrase': { type: 'string' },
    'app-login': { type: 'string' }, print: { type: 'boolean' }, help: { type: 'boolean' },
    'max-output-tokens': { type: 'string' },
    'reasoning-effort': { type: 'string' },
  } });
  if (values.help) { console.log(HELP); return; }
  let actionRef = values['action-ref'];
  if (!actionRef) {
    const root = fileURLToPath(new URL('../../', import.meta.url));
    const git = args => spawnSync('git', ['-C', root, ...args], { encoding: 'utf8' });
    const tracked = git(['cat-file', '-e', 'HEAD:scripts/github-action/setup.mjs']);
    const clean = git(['diff', '--quiet', 'HEAD', '--', 'action.yml', 'scripts/github-action']);
    const head = git(['rev-parse', 'HEAD']);
    if (tracked.status !== 0 || clean.status !== 0 || head.status !== 0) throw new Error('Use a clean Codewhale source checkout or supply --action-ref with a reviewed commit.');
    actionRef = head.stdout.trim();
  }
  const yaml = workflow({ provider: values.provider, model: values.model, version: values.version,
    actionRef, triggerPhrase: values['trigger-phrase'], appLogin: values['app-login'], maxOutputTokens: values['max-output-tokens'], reasoningEffort: values['reasoning-effort'] });
  if (values.print) { process.stdout.write(yaml); return; }
  const directory = realpathSync(values.directory || process.cwd());
  if (spawnSync('git', ['-C', directory, 'rev-parse', '--show-toplevel'], { encoding: 'utf8' }).stdout.trim() !== directory) throw new Error('--directory must be a Git repository root.');
  let current = directory;
  for (const name of ['.github', 'workflows']) {
    current = join(current, name);
    try { mkdirSync(current); } catch (error) { if (error.code !== 'EEXIST') throw error; }
    if (!lstatSync(current).isDirectory() || lstatSync(current).isSymbolicLink()) throw new Error('Workflow directories must be real directories.');
  }
  const output = join(current, 'codewhale-review.yml');
  writeFileSync(output, yaml, { flag: 'wx' });
  console.log(`Created ${output}\nSet the ${KEYS[values.provider]} Actions secret, then commit the workflow to your default branch.\n${values['app-login'] ? 'The publisher also needs CODEWHALE_APP_ID and CODEWHALE_APP_PRIVATE_KEY.\n' : ''}Reviews use ${values.provider}/${values.model}. GitHub runner usage and model usage follow your account limits.`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { setup(); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
