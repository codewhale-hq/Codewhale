import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const digest = value => createHash('sha256').update(value).digest('hex');
const upstreamFixes = [
  'https://github.com/isaacs/minimatch/commit/77a0f5c4b80bdc641c3c930a1d9edf5ad2db57f8',
  'https://github.com/isaacs/node-glob/commit/4682e8c9babee1b385202ede36389634e12076d7'
];
const reviewed = [
  {
    "package": "next",
    "version": "16.4.1",
    "file": "dist/compiled/@vercel/nft/index.js",
    "original": "e436d398b2263fb87a2d8ee3d9dab8db2544c684ba9bc1c0d4ab4e67acbab207",
    "patched": "3acafac86a359647efceec23f1a838103911dadd5ab63ee3a5b8afa661d20a37",
    "edits": [
      {
        "before": "firstPhasePreProcess(e){let t=false;",
        "after": "firstPhasePreProcess(e){const{braceExpandMax=100_000}=this.options;let t=false;",
        "count": 1
      },
      {
        "before": "if(!c||c===\".\"||c===\"..\"||!u||u===\".\"||u===\"..\"){continue}",
        "after": "if(!c||c===\".\"||c===\"..\"||!u||u===\".\"||u===\"..\"||e.length>=braceExpandMax){continue}",
        "count": 1
      },
      {
        "before": "secondPhasePreProcess(e){for(let t=0;",
        "after": "secondPhasePreProcess(e){if(e.length>1000)return e;for(let t=0;",
        "count": 1
      },
      {
        "before": "walkCB(e,t,s){if(this.signal?.aborted)s();",
        "after": "walkCB(e,t,s){if(this.signal?.aborted)return s();",
        "count": 1
      },
      {
        "before": "walkCB2(e,t,s,r){if(this.#p(e))return r();if(this.signal?.aborted)r();",
        "after": "walkCB2(e,t,s,r){if(this.#p(e))return r();if(this.signal?.aborted)return r();",
        "count": 1
      },
      {
        "before": "walkCBSync(e,t,s){if(this.signal?.aborted)s();",
        "after": "walkCBSync(e,t,s){if(this.signal?.aborted)return s();",
        "count": 1
      },
      {
        "before": "walkCB2Sync(e,t,s,r){if(this.#p(e))return r();if(this.signal?.aborted)r();",
        "after": "walkCB2Sync(e,t,s,r){if(this.#p(e))return r();if(this.signal?.aborted)return r();",
        "count": 1
      },
      {
        "before": "onResume(e){if(this.signal?.aborted)return;if(!this.paused)",
        "after": "onResume(e){if(!this.paused)",
        "count": 1
      }
    ]
  },
  {
    "package": "cf",
    "version": "1.0.0-beta.12",
    "file": "dist/autoconfig-PfnbLuEO.mjs",
    "original": "d0d55fbdc213153837647f7bf4bd03709d36b68b2f9ad8399800fafc0ff02eef",
    "patched": "daf62fbe10ebbd56171a9a51905b35f2f402312a27bc164699cbb6582eb57cc9",
    "edits": [
      {
        "before": "firstPhasePreProcess(e){let t=!1;",
        "after": "firstPhasePreProcess(e){const{braceExpandMax=100_000}=this.options;let t=!1;",
        "count": 1
      },
      {
        "before": "if(a!==`..`||!o||o===`.`||o===`..`||!s||s===`.`||s===`..`)continue;",
        "after": "if(a!==`..`||!o||o===`.`||o===`..`||!s||s===`.`||s===`..`||e.length>=braceExpandMax)continue;",
        "count": 1
      },
      {
        "before": "secondPhasePreProcess(e){for(let t=0;t<e.length-1;t++)",
        "after": "secondPhasePreProcess(e){if(e.length>1000)return e;for(let t=0;t<e.length-1;t++)",
        "count": 1
      }
    ]
  },
  {
    "package": "cf",
    "version": "1.0.0-beta.12",
    "file": "dist/d1-MvAQMvvo.mjs",
    "original": "0342fd94228a09739c49fe12234c37f9b69ef9b2e0181c7f2831fc9b675eb366",
    "patched": "c73b1da5735ab719be0dc90b7998559815797c9f4b1a6c1c77efc7723088f6eb",
    "edits": [
      {
        "before": "firstPhasePreProcess(e){let t=!1;",
        "after": "firstPhasePreProcess(e){const{braceExpandMax=100_000}=this.options;let t=!1;",
        "count": 1
      },
      {
        "before": "if(a!==`..`||!o||o===`.`||o===`..`||!s||s===`.`||s===`..`)continue;",
        "after": "if(a!==`..`||!o||o===`.`||o===`..`||!s||s===`.`||s===`..`||e.length>=braceExpandMax)continue;",
        "count": 1
      },
      {
        "before": "secondPhasePreProcess(e){for(let t=0;t<e.length-1;t++)",
        "after": "secondPhasePreProcess(e){if(e.length>1000)return e;for(let t=0;t<e.length-1;t++)",
        "count": 1
      }
    ]
  },
  {
    "package": "wrangler",
    "version": "4.147.0",
    "file": "wrangler-dist/cli.js",
    "original": "f2ec95b41d7f7665a42b75fc9631100e95ea6c0dbabc09eb65ae7f772dbc2fed",
    "patched": "ec5c069ff0bd70c9678a1e499f41bff15d94d5c09b7bacee4d9166ab30bcc63b",
    "edits": [
      {
        "before": "firstPhasePreProcess(globParts) {\n        let didSomething = false;",
        "after": "firstPhasePreProcess(globParts) {\n        const { braceExpandMax = 100_000 } = this.options;\n        let didSomething = false;",
        "count": 1
      },
      {
        "before": "if (!p8 || p8 === \".\" || p8 === \"..\" || !p22 || p22 === \".\" || p22 === \"..\") {",
        "after": "if (!p8 || p8 === \".\" || p8 === \"..\" || !p22 || p22 === \".\" || p22 === \"..\" || globParts.length >= braceExpandMax) {",
        "count": 1
      },
      {
        "before": "secondPhasePreProcess(globParts) {\n        for (let i8 = 0; i8 < globParts.length - 1; i8++) {",
        "after": "secondPhasePreProcess(globParts) {\n        if (globParts.length > 1000) return globParts;\n        for (let i8 = 0; i8 < globParts.length - 1; i8++) {",
        "count": 1
      }
    ]
  }
];

export function patchBundledGlobs({ root = resolve(dirname(fileURLToPath(import.meta.url)), '..'), checkOnly = false } = {}) {
  const lock = JSON.parse(readFileSync(join(root, 'package-lock.json'), 'utf8'));
  const pending = [];
  const copies = reviewed.map(entry => {
    const path = 'node_modules/' + entry.package;
    const pkg = JSON.parse(readFileSync(join(root, path, 'package.json'), 'utf8'));
    if (pkg.name !== entry.package || pkg.version !== entry.version || lock.packages?.[path]?.version !== pkg.version) throw new Error('Unreviewed bundled glob version or lock mismatch: ' + path);
    const target = join(root, path, entry.file);
    const source = readFileSync(target, 'utf8');
    const hash = digest(source);
    if (hash !== entry.patched) {
      if (checkOnly || hash !== entry.original) throw new Error('Unreviewed or unpatched glob bundle: ' + target);
      let patched = source;
      for (const edit of entry.edits) {
        if (patched.split(edit.before).length - 1 !== edit.count) throw new Error('Bundled glob patch anchor changed: ' + target);
        patched = patched.split(edit.before).join(edit.after);
      }
      if (digest(patched) !== entry.patched) throw new Error('Bundled glob patch digest mismatch: ' + target);
      pending.push({ path: target, source: patched });
    }
    return { package: entry.package, version: pkg.version, file: path + '/' + entry.file, sha256: entry.patched };
  });
  if (!checkOnly) for (const file of pending) writeFileSync(file.path, file.source);
  return { upstreamFixes, copies };
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  console.log(JSON.stringify({ mitigation: 'bounded bundled preprocessing and cancelled traversal', ...patchBundledGlobs() }));
}
