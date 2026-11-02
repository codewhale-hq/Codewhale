// Raw upstream findings remain in the report. Only the exact reviewed and
// installed mitigations admit their transitive build-tool findings.
import { spawnSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { patchBraces } from './patch-braces.mjs';
import { patchMinimatch } from './patch-minimatch.mjs';
import { patchGlob } from './patch-glob.mjs';
import { patchBundledGlobs } from './patch-bundled-globs.mjs';

const advisory = 'https://github.com/advisories/GHSA-vfj7-8cjw-p6xm';
const minimatchAdvisory = 'https://github.com/advisories/GHSA-g3hv-75wg-m249';
const globAdvisory = 'https://github.com/advisories/GHSA-3g3m-2fjr-6pw4';

export function classifyAudit(report) {
  if (report.error || !report.vulnerabilities || !report.metadata?.vulnerabilities) throw new Error('Incomplete dependency audit');
  const entries = report.vulnerabilities;
  if (Object.keys(entries).length !== report.metadata.vulnerabilities.total) throw new Error('Dependency audit count mismatch');
  function mitigated(name) {
    const pending = [name], visited = new Set();
    let concrete = false;
    while (pending.length) {
      const current = pending.pop();
      if (visited.has(current)) continue;
      visited.add(current);
      const entry = entries[current];
      if (!entry || !Array.isArray(entry.via) || !entry.via.length) return false;
      for (const via of entry.via) {
        if (typeof via === 'string') pending.push(via);
        else {
          concrete = true;
          if (!((current === 'braces' && via.name === 'braces' && via.url === advisory)
            || (current === 'minimatch' && via.name === 'minimatch' && via.url === minimatchAdvisory)
            || (current === 'glob' && via.name === 'glob' && via.url === globAdvisory))) return false;
        }
      }
    }
    return concrete;
  }
  // A nested second copy would bypass the single reviewed installation patch.
  const braces = entries.braces;
  if (braces && (braces.nodes?.length !== 1 || braces.nodes[0] !== 'node_modules/braces')) throw new Error('Unreviewed nested braces dependency');
  const minimatch = entries.minimatch;
  if (minimatch) {
    const reviewed = new Set(patchMinimatch({ checkOnly: true }).copies.map(copy => copy.path));
    if (!minimatch.nodes?.length || minimatch.nodes.some(path => !reviewed.has(path))) throw new Error('Unreviewed nested minimatch dependency');
  }
  const glob = entries.glob;
  if (glob) {
    const reviewed = new Set(patchGlob({ checkOnly: true }).copies.map(copy => copy.path));
    if (!glob.nodes?.length || glob.nodes.some(path => !reviewed.has(path))) throw new Error('Unreviewed nested glob dependency');
  }
  const blocked = Object.keys(entries).filter(name => !mitigated(name));
  return { upstreamFindings: report.metadata.vulnerabilities.total, mitigated: Object.keys(entries).filter(name => !blocked.includes(name)), blocked };
}

function main() {
  const proof = patchBraces({ checkOnly: true });
  const minimatchProof = patchMinimatch({ checkOnly: true });
  const globProof = patchGlob({ checkOnly: true });
  const bundledProof = patchBundledGlobs({ checkOnly: true });
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
  console.log(JSON.stringify({ ...verdict, reviewedMitigation: proof, minimatchMitigation: minimatchProof, globMitigation: globProof, bundledGlobMitigation: bundledProof }));
  if (verdict.blocked.length) process.exitCode = 1;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
