import { spawnSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const advisory = 'https://github.com/advisories/GHSA-xcx6-jx8p-x3h5';

export function classifyAudit(report) {
  if (report.error || !report.vulnerabilities || !report.metadata?.vulnerabilities) throw new Error('Incomplete dependency audit');
  const entries = report.vulnerabilities;
  if (Object.keys(entries).length !== report.metadata.vulnerabilities.total) throw new Error('Dependency audit count mismatch');
  if (report.metadata.vulnerabilities.critical) throw new Error('Critical dependency vulnerability');
  function accepted(name, seen = new Set()) {
    if (seen.has(name)) return false;
    const entry = entries[name];
    if (!entry || !Array.isArray(entry.via) || !entry.via.length) return false;
    const next = new Set([...seen, name]);
    return entry.via.every(via => typeof via === 'string'
      ? accepted(via, next)
      : name === '@azure/msal-node' && via.name === '@azure/msal-node' && via.url === advisory && via.severity === 'high');
  }
  const nodes = entries['@azure/msal-node']?.nodes ?? [];
  if (nodes.length > 1 || (nodes.length === 1 && nodes[0] !== 'node_modules/@azure/msal-node')) throw new Error('Unreviewed @azure/msal-node installation');
  const blocked = Object.keys(entries).filter(name => !accepted(name));
  return { upstreamFindings: report.metadata.vulnerabilities.total, accepted: Object.keys(entries).filter(name => !blocked.includes(name)), blocked };
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
  console.log(JSON.stringify({ ...verdict, reviewedAcceptance: advisory }));
  if (verdict.blocked.length) process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
