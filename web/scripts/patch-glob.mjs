import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const digest = value => createHash('sha256').update(value).digest('hex');
const advisory = 'https://github.com/advisories/GHSA-3g3m-2fjr-6pw4';
const upstreamFix = 'https://github.com/isaacs/node-glob/commit/4682e8c9babee1b385202ede36389634e12076d7';
const reviewed = {
  "9.3.5": [
    {
      "file": "dist/cjs/walker.js",
      "original": "b3a16383ccc1312ea12ff3b834fad911bce5420856cad639d9e169ad6e2c05a2",
      "patched": "dcfeed010a9ef371933e2981a10bc0e9d92b849f09c7d64bf9cc0197b1571dfe"
    },
    {
      "file": "dist/mjs/walker.js",
      "original": "7b81b11d8cde0d728eafd147aad9f949eee8511b7ca5bb801b4135f5812e9332",
      "patched": "07fcd868bab53cc288da4ada0998fc3cb6dd7c14ca83cc9ae32a33bb550dbe4e"
    }
  ],
  "12.0.0": [
    {
      "file": "dist/commonjs/walker.js",
      "original": "1138ced9d5429681a1740af32d13391a38184a3376d1d9d56eea64b7a43274a1",
      "patched": "4eca6e1f2fd9c9ef7469977a12c7e9bd6503bc7a9257ccdc8e79b346ec34fccd"
    },
    {
      "file": "dist/esm/walker.js",
      "original": "3e15af549ecc74d8691982d571168fd4b0e38b494c2cbb3edcfb8f97d4036ac6",
      "patched": "8afedbdf3bb2bddaf37ee3ef4b393b76a5a99c80ef12aaedd2160a77c2effadb"
    }
  ]
};
const edits = [
  {
    "before": "        if (this.signal?.aborted)\n            cb();",
    "after": "        if (this.signal?.aborted)\n            return cb();",
    "count": 4
  },
  {
    "before": "    onResume(fn) {\n        if (this.signal?.aborted)\n            return;",
    "after": "    onResume(fn) {",
    "count": 1
  }
];

export function patchGlob({ root = resolve(dirname(fileURLToPath(import.meta.url)), '..'), checkOnly = false } = {}) {
  const lock = JSON.parse(readFileSync(join(root, 'package-lock.json'), 'utf8'));
  const copies = Object.entries(lock.packages ?? {}).filter(([path]) => path.startsWith('node_modules/') && path.endsWith('/glob'));
  if (!copies.length) throw new Error('Missing reviewed glob dependency copies');
  const pending = [];
  const receipt = copies.map(([path, entry]) => {
    if (path.split('/').includes('..')) throw new Error('Invalid glob dependency path');
    const pkg = JSON.parse(readFileSync(join(root, path, 'package.json'), 'utf8'));
    const files = reviewed[pkg.version];
    if (pkg.name !== 'glob' || pkg.version !== entry.version || !files) throw new Error('Unreviewed glob version or lock mismatch: ' + path);
    return { path, version: pkg.version, files: files.map(file => {
      const target = join(root, path, file.file);
      const source = readFileSync(target, 'utf8');
      const hash = digest(source);
      if (hash !== file.patched) {
        if (checkOnly || hash !== file.original) throw new Error('Unreviewed or unpatched glob file: ' + target);
        let patched = source;
        for (const edit of edits) {
          if (patched.split(edit.before).length - 1 !== edit.count) throw new Error('glob patch anchor changed: ' + target);
          patched = patched.split(edit.before).join(edit.after);
        }
        if (digest(patched) !== file.patched) throw new Error('glob patch digest mismatch: ' + target);
        pending.push({ path: target, source: patched });
      }
      return { file: file.file, sha256: file.patched };
    }) };
  });
  if (!checkOnly) for (const file of pending) writeFileSync(file.path, file.source);
  return { advisory, upstreamFix, copies: receipt };
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  console.log(JSON.stringify({ mitigation: 'stop filesystem traversal on abort', ...patchGlob() }));
}
