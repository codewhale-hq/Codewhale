// Actual Windows Terminal clipboard acceptance on disposable GitHub runners.
// Uses a disposable home and the existing draft stash; no inference or credentials.
// This qualifies clipboard-to-draft delivery in the runner's interactive session, not
// every Windows desktop, IME, remote-desktop client or terminal distribution.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs/promises';
import { appendFileSync } from 'node:fs';
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
const expected = Array.from({ length: 24 }, (_, n) => `cw-clipboard-${String(n + 1).padStart(2, '0')} 漢字 🐳 line ${n + 1}`).join('\n');
const stashFile = path.join(qaHome, '.codewhale', 'composer_stash.jsonl');
const receipt = { source: process.env.GITHUB_SHA, binary, result: 'failed', scope: 'Windows Terminal clipboard-to-stashed-draft', lineCount: 24, harnessSubmitKeys: 0, title };
let desktop;
let desktopClosed;
let desktopEnv;
let terminal;
let desktopError = '';

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
  assert.equal(await fs.lstat(stashFile).then(() => true, error => { if (error.code === 'ENOENT') return false; throw error; }), false, 'fresh profile has no preexisting draft stash');
  await fs.writeFile(path.join(evidence, 'expected.txt'), expected.replaceAll('\n', '\r\n'));
  // Official Microsoft ZIP, pinned by release and GitHub's asset SHA-256.
  const terminalUrl = 'https://github.com/microsoft/terminal/releases/download/v1.25.2733.0/Microsoft.WindowsTerminal_1.25.2733.0_x64.zip';
  const terminalHash = 'bf3ef2012f6c44d8340a4c58125acc9498d19b580f9890dc043cdf831852e796';
  let bytes;
  for (let attempt = 1; attempt <= 5; attempt += 1) {
    try {
      const download = await fetch(terminalUrl, { signal: AbortSignal.timeout(180_000) });
      assert.equal(download.status, 200, 'official Terminal download');
      const candidate = Buffer.from(await download.arrayBuffer());
      assert.equal(createHash('sha256').update(candidate).digest('hex'), terminalHash, 'Terminal asset digest');
      bytes = candidate;
      break;
    } catch (error) {
      if (attempt === 5) throw error;
      await new Promise(resolve => setTimeout(resolve, attempt * 15_000));
    }
  }
  const zip = path.join(root, 'terminal.zip'); await fs.writeFile(zip, bytes);
  await fs.mkdir(terminalRoot);
  execFileSync('tar.exe', ['-xf', zip, '-C', terminalRoot], { timeout: 60_000 });
  const files = await fs.readdir(terminalRoot, { recursive: true });
  const executables = files.filter(f => path.basename(f).toLowerCase() === 'windowsterminal.exe');
  assert.equal(executables.length, 1, 'unique portable Terminal executable');
  terminal = path.join(terminalRoot, executables[0]);
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
    actions: [{ command: 'paste', keys: 'shift+insert' }],
  }, null, 2));
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => /^(PATH|SYSTEMROOT|WINDIR|COMSPEC|TEMP|TMP|PATHEXT|SYSTEMDRIVE|PROGRAMFILES|PROGRAMFILES\(X86\)|PROGRAMW6432|PROCESSOR_ARCHITECTURE|NUMBER_OF_PROCESSORS)$/i.test(k)));
  Object.assign(env, {
    HOME: qaHome, USERPROFILE: qaHome, APPDATA: path.join(qaHome, 'AppData', 'Roaming'), LOCALAPPDATA: path.join(qaHome, 'AppData', 'Local'),
    XDG_CONFIG_HOME: path.join(qaHome, '.config'), XDG_DATA_HOME: path.join(qaHome, '.local', 'share'), XDG_CACHE_HOME: path.join(qaHome, '.cache'),
    CODEWHALE_CONFIG_PATH: path.join(qaHome, '.codewhale', 'config.toml'), DEEPSEEK_CONFIG_PATH: path.join(qaHome, '.deepseek', 'config.toml'),
    CODEWHALE_TELEMETRY: '0', CODEWHALE_NO_UPDATE_CHECK: '1', CODEWHALE_DISABLE_MODELS_DEV_FETCH: '1', CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE: '1', NO_ANIMATIONS: '1',
  });
  desktopEnv = env;
  receipt.terminal = { version: '1.25.2733.0', sha256: terminalHash, distribution: 'official portable x64' };
  const script = fileURLToPath(new URL('./windows-terminal-clipboard.ps1', import.meta.url));
  receipt.desktop = { requestedAt: new Date().toISOString() };
  desktop = spawn('powershell.exe', ['-NoProfile', '-STA', '-File', script, terminal, binary, workspace, title, evidence], { env, windowsHide: false, stdio: ['ignore', 'pipe', 'pipe'] });
  desktop.stdout.on('data', data => { try { appendFileSync(path.join(evidence, 'desktop.log'), data); } catch (error) { receipt.desktop.logError = String(error); desktopError += String(error); } });
  desktop.stderr.on('data', data => { desktopError += data; try { appendFileSync(path.join(evidence, 'desktop-error.log'), data); } catch (error) { receipt.desktop.logError = String(error); desktopError += String(error); } });
  desktop.once('spawn', () => { receipt.desktop.pid = desktop.pid; receipt.desktop.spawnedAt = new Date().toISOString(); });
  desktopClosed = new Promise(resolve => {
    desktop.once('error', error => { receipt.desktop.error = String(error); desktopError += String(error); });
    desktop.once('exit', (code, signal) => { receipt.desktop.exit = { code, signal, at: new Date().toISOString() }; });
    desktop.once('close', code => { receipt.desktop.closedAt = new Date().toISOString(); resolve(code); });
  });
  await until(() => exists('ready.json'), 'Windows Terminal composer readiness', 120_000);
  receipt.readyAt = new Date().toISOString();
  await until(() => exists('pasted.json'), 'actual clipboard paste');
  receipt.pastedAt = new Date().toISOString();
  await fs.writeFile(path.join(evidence, 'stash'), 'stash once');
  await until(() => exists('completed.json'), 'actual composer stash');
  const metadata = await fs.lstat(stashFile);
  assert.ok(metadata.isFile() && !metadata.isSymbolicLink(), 'draft stash is a regular file');
  assert.ok(metadata.size <= 64 * 1024, 'draft stash stays bounded');
  const stashBytes = await fs.readFile(stashFile);
  const lines = stashBytes.toString('utf8').split(/\r?\n/);
  if (lines.at(-1) === '') lines.pop();
  assert.equal(lines.length, 1, 'exactly one draft stash record');
  const draft = JSON.parse(lines[0]);
  assert.ok(draft && typeof draft === 'object' && !Array.isArray(draft) && typeof draft.text === 'string' && typeof draft.ts === 'string', 'draft stash record has text and timestamp');
  assert.ok(Number.isFinite(Date.parse(draft.ts)), 'draft stash timestamp is valid');
  const normalized = draft.text.replaceAll('\r\n', '\n');
  assert.equal(normalized, expected, 'all24 Unicode lines are stashed exactly');
  for (let line = 1; line <= 24; line++) assert.equal(normalized.split(`cw-clipboard-${String(line).padStart(2, '0')}`).length - 1, 1, `line${line} occurs once`);
  await fs.writeFile(path.join(evidence, 'composer_stash.jsonl'), stashBytes);
  receipt.stash = { path: stashFile, sha256: createHash('sha256').update(stashBytes).digest('hex'), records: 1, exactDraftMatch: true, normalization: 'CRLF to LF only' };
  assert.equal(await desktopClosed, 0, desktopError);
  assert.equal(desktopError, '', 'no desktop errors');
  receipt.cleanup = JSON.parse((await fs.readFile(path.join(evidence, 'cleanup.json'), 'utf8')).replace(/^\uFEFF/, ''));
  assert.equal(receipt.cleanup.terminal, terminal, 'cleanup used the owned Terminal path');
  assert.equal(receipt.cleanup.remaining, 0, 'owned Terminal processes are gone');
  receipt.result = 'passed';
} catch (error) {
  receipt.error = String(error); process.exitCode = 1;
} finally {
  if (desktop) {
    receipt.desktop.beforeCleanup = { exitCode: desktop.exitCode, signalCode: desktop.signalCode, at: new Date().toISOString() };
    if (desktop.exitCode === null && desktop.signalCode === null) {
      receipt.desktop.cancelRequestedAt = new Date().toISOString();
      try {
        await fs.writeFile(path.join(evidence, 'cancel'), 'retire owned Terminal');
        await Promise.race([desktopClosed, delay(5000)]);
      } catch (error) { receipt.desktop.cancelError = String(error); }
    }
    if (receipt.result !== 'passed') {
      try {
        const cleanup = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
          '$ErrorActionPreference = "Stop"; $terminal = $env:CODEWHALE_CLIPBOARD_TERMINAL; $stopped = @(); foreach ($process in @(Get-Process | Where-Object { $_.Path -eq $terminal })) { Stop-Process -InputObject $process -Force; if (!$process.WaitForExit(5000)) { throw "Owned Terminal did not exit" }; $stopped += $process.Id }; $remaining = @(Get-Process | Where-Object { $_.Path -eq $terminal }).Count; if ($remaining) { throw "Owned Terminal is still running" }; @{ terminal = $terminal; stopped = $stopped; remaining = $remaining } | ConvertTo-Json -Compress'],
        { env: { ...desktopEnv, CODEWHALE_CLIPBOARD_TERMINAL: terminal }, encoding: 'utf8', timeout: 10_000 });
        receipt.desktop.fallbackCleanup = JSON.parse(cleanup.replace(/^\uFEFF/, ''));
        assert.equal(receipt.desktop.fallbackCleanup.terminal, terminal, 'fallback cleanup used the owned Terminal path');
        assert.equal(receipt.desktop.fallbackCleanup.remaining, 0, 'fallback cleanup retired the owned Terminal');
      } catch (error) {
        receipt.desktop.cleanupError = String(error); receipt.result = 'failed'; process.exitCode = 1;
      }
    }
    if (desktop.exitCode === null && desktop.signalCode === null) {
      receipt.desktop.killRequestedAt = new Date().toISOString(); desktop.kill();
    }
    await desktopClosed;
  }
  await fs.writeFile(path.join(evidence, 'receipt.json'), JSON.stringify(receipt, null, 2));
  console.log(JSON.stringify(receipt));
}
