// Actual Windows Terminal clipboard acceptance on disposable GitHub runners.
// Uses a synthetic home and loopback provider; no credentials or paid inference.
// This qualifies clipboard delivery in the runner's interactive session, not
// every Windows desktop, IME, remote-desktop client or terminal distribution.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs/promises';
import { appendFileSync } from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

assert.equal(process.platform, 'win32', 'requires a real Windows desktop');
assert.equal(process.env.GITHUB_ACTIONS, 'true', 'disposable GitHub runner only');
const [binaryArg, evidenceArg] = process.argv.slice(2);
assert.ok(binaryArg && evidenceArg, 'usage: node windows-terminal-clipboard.mjs BINARY EVIDENCE_DIR');
const binary = path.resolve(binaryArg);
const evidence = path.resolve(evidenceArg);
await fs.mkdir(evidence, { recursive: true });
const root = await fs.mkdtemp(path.join(process.env.RUNNER_TEMP, 'cw-terminal-fixture-'));
const qaHome = path.join(root, 'home');
const workspace = path.join(root, 'workspace');
const terminalRoot = path.join(root, 'terminal');
const title = `Codewhale clipboard ${randomUUID()}`;
const model = 'gpt-4o-mini';
const expected = Array.from({ length: 24 }, (_, n) => `cw-clipboard-${String(n + 1).padStart(2, '0')} 漢字 🐳 line ${n + 1}`).join('\n');
const requests = [];
const receipt = { source: process.env.GITHUB_SHA, binary, result: 'failed', lineCount: 24, provider: 'loopback fixture', title };
let desktop;
let desktopExit;
let desktopError = '';
const server = http.createServer(async (req, res) => {
  if (req.method === 'GET' && req.url === '/v1/models') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ object: 'list', data: [{ id: model, object: 'model', owned_by: 'fixture' }] }));
    return;
  }
  if (req.method !== 'POST' || req.url !== '/v1/chat/completions') {
    res.writeHead(404); res.end(); return;
  }
  try {
    const chunks = []; let size = 0;
    for await (const chunk of req) {
      size += chunk.length;
      assert.ok(size <= 2 * 1024 * 1024, 'fixture request exceeds limit');
      chunks.push(chunk);
    }
    const body = JSON.parse(Buffer.concat(chunks));
    const users = (body.messages ?? []).filter(m => m.role === 'user').map(m => typeof m.content === 'string'
      ? m.content : (m.content ?? []).filter(c => c.type === 'text').map(c => c.text).join('\n'));
    requests.push(users);
    // Retain only synthetic user input; never dump headers or system prompts.
    await fs.writeFile(path.join(evidence, 'provider-user-messages.json'), JSON.stringify(requests, null, 2));
    const response = { id: 'clipboard-fixture', object: 'chat.completion', created: 1, model,
      choices: [{ index: 0, message: { role: 'assistant', content: 'cw-clipboard-ok' }, finish_reason: 'stop' }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } };
    if (body.stream) {
      res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
      res.write(`data: ${JSON.stringify({ ...response, object: 'chat.completion.chunk', choices: [{ index: 0, delta: { role: 'assistant', content: 'cw-clipboard-ok' }, finish_reason: null }] })}\n\n`);
      res.write(`data: ${JSON.stringify({ ...response, object: 'chat.completion.chunk', choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`);
      res.end('data: [DONE]\n\n');
    } else {
      res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(response));
    }
  } catch (error) { desktopError ||= String(error); res.writeHead(500); res.end(); }
});

async function until(check, label, milliseconds = 60_000) {
  const deadline = Date.now() + milliseconds;
  while (Date.now() < deadline) {
    const result = await check();
    if (result) return result;
    assert.ok(!receipt.desktop?.error && desktop?.exitCode === null && desktop?.signalCode === null, `desktop stopped while waiting for ${label}: ${desktopError}`);
    await delay(100);
  }
  throw new Error(`deadline waiting for ${label}: ${desktopError}`);
}
const exists = async name => fs.access(path.join(evidence, name)).then(() => true, () => false);
try {
  receipt.binarySha256 = createHash('sha256').update(await fs.readFile(binary)).digest('hex');
  receipt.version = execFileSync(binary, ['--version'], { encoding: 'utf8' }).trim();
  for (const dir of [workspace, path.join(qaHome, '.codewhale'), path.join(qaHome, '.deepseek'), path.join(qaHome, 'AppData', 'Roaming'), path.join(qaHome, 'AppData', 'Local')]) await fs.mkdir(dir, { recursive: true });
  const config = 'telemetry = false\n[notifications]\nmethod = "off"\ncompletion_sound = "off"\n';
  for (const folder of ['.codewhale', '.deepseek']) await fs.writeFile(path.join(qaHome, folder, 'config.toml'), config);
  await fs.writeFile(path.join(evidence, 'expected.txt'), expected.replaceAll('\n', '\r\n'));
  // Official Microsoft ZIP, pinned by release and GitHub's asset SHA-256.
  const terminalUrl = 'https://github.com/microsoft/terminal/releases/download/v1.25.2733.0/Microsoft.WindowsTerminal_1.25.2733.0_x64.zip';
  const terminalHash = 'bf3ef2012f6c44d8340a4c58125acc9498d19b580f9890dc043cdf831852e796';
  const download = await fetch(terminalUrl, { signal: AbortSignal.timeout(60_000) });
  assert.equal(download.status, 200, 'official Terminal download');
  const bytes = Buffer.from(await download.arrayBuffer());
  assert.equal(createHash('sha256').update(bytes).digest('hex'), terminalHash, 'Terminal asset digest');
  const zip = path.join(root, 'terminal.zip'); await fs.writeFile(zip, bytes);
  await fs.mkdir(terminalRoot);
  execFileSync('tar.exe', ['-xf', zip, '-C', terminalRoot], { timeout: 60_000 });
  const files = await fs.readdir(terminalRoot, { recursive: true });
  const executables = files.filter(f => path.basename(f).toLowerCase() === 'windowsterminal.exe');
  assert.equal(executables.length, 1, 'unique portable Terminal executable');
  const terminal = path.join(terminalRoot, executables[0]);
  const terminalDir = path.dirname(terminal);
  await fs.writeFile(path.join(terminalDir, '.portable'), '');
  await fs.mkdir(path.join(terminalDir, 'settings'), { recursive: true });
  const profile = '{9e4b713f-d69c-4206-85fb-98663b729f17}';
  await fs.writeFile(path.join(terminalDir, 'settings', 'settings.json'), JSON.stringify({
    defaultProfile: profile, initialCols: 110, initialRows: 38, confirmCloseAllTabs: false,
    // The payload is trusted fixture text; this tests Codewhale's paste path,
    // excluding Terminal's own optional multiline confirmation dialog.
    multiLinePasteWarning: false, largePasteWarning: false,
    profiles: { defaults: { font: { size: 12 } }, list: [{ guid: profile, name: 'Codewhale fixture', commandline: 'cmd.exe' }] },
    actions: [{ command: 'paste', keys: 'ctrl+shift+alt+v' }],
  }, null, 2));
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const baseUrl = `http://127.0.0.1:${server.address().port}/v1`;
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => /^(PATH|SYSTEMROOT|WINDIR|COMSPEC|TEMP|TMP|PATHEXT|SYSTEMDRIVE|PROGRAMFILES|PROGRAMFILES\(X86\)|PROGRAMW6432|PROCESSOR_ARCHITECTURE|NUMBER_OF_PROCESSORS)$/i.test(k)));
  Object.assign(env, {
    HOME: qaHome, USERPROFILE: qaHome, APPDATA: path.join(qaHome, 'AppData', 'Roaming'), LOCALAPPDATA: path.join(qaHome, 'AppData', 'Local'),
    XDG_CONFIG_HOME: path.join(qaHome, '.config'), XDG_DATA_HOME: path.join(qaHome, '.local', 'share'), XDG_CACHE_HOME: path.join(qaHome, '.cache'),
    CODEWHALE_CONFIG_PATH: path.join(qaHome, '.codewhale', 'config.toml'), DEEPSEEK_CONFIG_PATH: path.join(qaHome, '.deepseek', 'config.toml'),
    CODEWHALE_TELEMETRY: '0', CODEWHALE_NO_UPDATE_CHECK: '1', CODEWHALE_DISABLE_MODELS_DEV_FETCH: '1', CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE: '1', NO_ANIMATIONS: '1',
  });
  receipt.terminal = { version: '1.25.2733.0', sha256: terminalHash, distribution: 'official portable x64' };
  const script = fileURLToPath(new URL('./windows-terminal-clipboard.ps1', import.meta.url));
  receipt.desktop = { requestedAt: new Date().toISOString() };
  desktop = spawn('powershell.exe', ['-NoProfile', '-STA', '-File', script, terminal, binary, workspace, baseUrl, title, evidence], { env, windowsHide: false, stdio: ['ignore', 'pipe', 'pipe'] });
  desktop.stdout.on('data', data => { try { appendFileSync(path.join(evidence, 'desktop.log'), data); } catch (error) { receipt.desktop.logError = String(error); desktopError += String(error); } });
  desktop.stderr.on('data', data => { desktopError += data; try { appendFileSync(path.join(evidence, 'desktop-error.log'), data); } catch (error) { receipt.desktop.logError = String(error); desktopError += String(error); } });
  desktop.once('spawn', () => { receipt.desktop.pid = desktop.pid; receipt.desktop.spawnedAt = new Date().toISOString(); });
  desktop.once('close', () => { receipt.desktop.closedAt = new Date().toISOString(); });
  desktopExit = new Promise(resolve => {
    desktop.once('error', error => { receipt.desktop.error = String(error); desktopError += String(error); resolve(null); });
    desktop.once('exit', (code, signal) => { receipt.desktop.exit = { code, signal, at: new Date().toISOString() }; resolve(code); });
  });
  await until(() => exists('ready.json'), 'Windows Terminal composer readiness', 120_000);
  receipt.readyAt = new Date().toISOString();
  await until(() => exists('pasted.json'), 'actual clipboard paste');
  receipt.pastedAt = new Date().toISOString();
  assert.equal(requests.length, 0, 'pasting must not submit any partial turn');
  receipt.requestsBeforeEnter = 0;
  await fs.writeFile(path.join(evidence, 'submit'), 'enter once');
  await until(() => exists('completed.json'), 'rendered fixture reply');
  await delay(1000);
  assert.equal(requests.length, 1, 'one Enter must submit exactly one turn');
  const matching = requests[0].filter(text => text.includes('cw-clipboard-01'));
  assert.equal(matching.length, 1, 'one user message contains the pasted input');
  const normalized = matching[0].replaceAll('\r\n', '\n');
  assert.ok(normalized.includes(expected), 'all24 Unicode lines arrive contiguously and unchanged');
  for (let line = 1; line <= 24; line++) assert.equal(normalized.split(`cw-clipboard-${String(line).padStart(2, '0')}`).length - 1, 1, `line${line} occurs once`);
  assert.equal(await desktopExit, 0, desktopError);
  assert.equal(desktopError, '', 'no desktop or provider fixture errors');
  receipt.requestsAfterEnter = 1;
  receipt.result = 'passed';
} catch (error) {
  receipt.error = String(error); process.exitCode = 1;
} finally {
  if (desktop) {
    receipt.desktop.beforeCleanup = { exitCode: desktop.exitCode, signalCode: desktop.signalCode, at: new Date().toISOString() };
    if (desktop.exitCode === null && desktop.signalCode === null) {
      receipt.desktop.killRequestedAt = new Date().toISOString(); desktop.kill();
    }
    await desktopExit;
  }
  server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
  await fs.writeFile(path.join(evidence, 'receipt.json'), JSON.stringify(receipt, null, 2));
  console.log(JSON.stringify(receipt));
}
