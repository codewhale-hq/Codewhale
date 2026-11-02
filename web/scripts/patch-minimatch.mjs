import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const digest = value => createHash('sha256').update(value).digest('hex');
const advisory = 'https://github.com/advisories/GHSA-g3hv-75wg-m249';
const upstreamFix = 'https://github.com/isaacs/minimatch/commit/77a0f5c4b80bdc641c3c930a1d9edf5ad2db57f8';
const reviewed = {
  "3.1.5": [
    {
      "file": "minimatch.js",
      "original": "746849f267c4868a53729225bd85c7dccd387712136fb305fcc0a3eadd4a3d9d",
      "patched": "746849f267c4868a53729225bd85c7dccd387712136fb305fcc0a3eadd4a3d9d"
    }
  ],
  "8.0.7": [
    {
      "file": "dist/cjs/index.js",
      "original": "0bdc0f17d1400f5778f6bb2080f2b8577a6476234ad19cfaead4963fd60d9e67",
      "patched": "cdb6db6057c1c7dc67b056850666fc2c62c23f8957a05b913fa4050ba9fb748f"
    },
    {
      "file": "dist/mjs/index.js",
      "original": "71bb425df4c51d291c51b3bebcb545f393b0ba8790ca71b68a3f2ae63bb96400",
      "patched": "680e509ca7710b38192e7d439d385b818e258331058b517096fa6ea688818842"
    }
  ],
  "10.2.5": [
    {
      "file": "dist/commonjs/index.js",
      "original": "d224f71f20d7c206fcc5b7e9f747324c7698345a0b2c28d717dd9cf698306783",
      "patched": "7e835c0a30f7b4f29f4ad8cf8c3e92b97ab23ed854a7a777f240d6bdf3d19231"
    },
    {
      "file": "dist/esm/index.js",
      "original": "59821ec2ce0d3df968292036e5c831ad322b33e9c33a949ecbc9dfe125d165be",
      "patched": "37ba4ff4010cda499e890975983b78ebd5f2be2dd8506d938472de4af8e9ed25"
    }
  ]
};
const edits = [
  {
    "before": "    firstPhasePreProcess(globParts) {\n        let didSomething = false;",
    "after": "    firstPhasePreProcess(globParts) {\n        const { braceExpandMax = 100_000 } = this.options;\n        let didSomething = false;",
    "count": 1
  },
  {
    "before": "                        p2 === '.' ||\n                        p2 === '..') {",
    "after": "                        p2 === '.' ||\n                        p2 === '..' ||\n                        globParts.length >= braceExpandMax) {",
    "count": 1
  },
  {
    "before": "    secondPhasePreProcess(globParts) {\n        for (let i = 0; i < globParts.length - 1; i++) {",
    "after": "    secondPhasePreProcess(globParts) {\n        if (globParts.length > 1000) return globParts;\n        for (let i = 0; i < globParts.length - 1; i++) {",
    "count": 1
  }
];

export function patchMinimatch({ root = resolve(dirname(fileURLToPath(import.meta.url)), '..'), checkOnly = false } = {}) {
  const lock = JSON.parse(readFileSync(join(root, 'package-lock.json'), 'utf8'));
  const copies = Object.entries(lock.packages ?? {}).filter(([path]) => path.startsWith('node_modules/') && path.endsWith('/minimatch'));
  if (!copies.length) throw new Error('Missing reviewed minimatch dependency copies');
  const pending = [];
  const receipt = copies.map(([path, entry]) => {
    if (path.split('/').includes('..')) throw new Error('Invalid minimatch dependency path');
    const pkg = JSON.parse(readFileSync(join(root, path, 'package.json'), 'utf8'));
    const files = reviewed[pkg.version];
    if (pkg.name !== 'minimatch' || pkg.version !== entry.version || !files) throw new Error('Unreviewed minimatch version or lock mismatch: ' + path);
    return { path, version: pkg.version, files: files.map(file => {
      const target = join(root, path, file.file);
      const source = readFileSync(target, 'utf8');
      const hash = digest(source);
      if (hash !== file.patched) {
        if (checkOnly || hash !== file.original) throw new Error('Unreviewed or unpatched minimatch file: ' + target);
        let patched = source;
        for (const edit of edits) {
          if (patched.split(edit.before).length - 1 !== edit.count) throw new Error('minimatch patch anchor changed: ' + target);
          patched = patched.split(edit.before).join(edit.after);
        }
        if (digest(patched) !== file.patched) throw new Error('minimatch patch digest mismatch: ' + target);
        pending.push({ path: target, source: patched });
      }
      return { file: file.file, sha256: file.patched };
    }) };
  });
  if (!checkOnly) for (const file of pending) writeFileSync(file.path, file.source);
  return { advisory, upstreamFix, copies: receipt };
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  console.log(JSON.stringify({ mitigation: 'bounded globparts preprocessing', ...patchMinimatch() }));
}
