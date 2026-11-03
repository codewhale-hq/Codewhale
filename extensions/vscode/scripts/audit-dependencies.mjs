import { spawnSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export function classifyAudit(report) {
  if (report.error || !report.vulnerabilities || !report.metadata?.vulnerabilities) throw new Error('Incomplete dependency audit');
  const entries = report.vulnerabilities;
  if (Object.keys(entries).length !== report.metadata.vulnerabilities.total) throw new Error('Dependency audit count mismatch');
  if (report.metadata.vulnerabilities.critical) throw new Error('Critical dependency vulnerability');
  return { upstreamFindings: report.metadata.vulnerabilities.total, blocked: Object.keys(entries) };
}

function main() {
  if (!process.env.npm_execpath) throw new Error('Run through npm run audit:dependencies');
  const result = spawnSync(process.execPath, [process.env.npm_execpath, 'audit', '--json'], { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
  if (result.error || result.signal || ![0, 1].includes(result.status)) throw new Error('Dependency audit failed to complete');
  const report = JSON.parse(result.stdout);
  const at = process.argv.indexOf('--report');
  if (at >= 0) {
    if (!process.argv[at + 1]) throw new Error('Missing audit report path');
    writeFileSync(process.argv[at + 1], JSON.stringify(report, null, 2) + '\n');
  }
  const verdict = classifyAudit(report);
  console.log(JSON.stringify(verdict));
  if (verdict.blocked.length) process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
