#!/usr/bin/env node
/** Local TypeScript build plus deterministic embedded-runtime packaging. */
import { readFile, writeFile, cp, mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname } from 'node:path';
import { spawnSync } from 'node:child_process';
import ts from 'typescript';
const root = fileURLToPath(new URL('../', import.meta.url));
const result = spawnSync(process.execPath, [root + 'node_modules/typescript/bin/tsc', '-p', root + 'tsconfig.json'], { stdio: 'inherit' });
if (result.status !== 0) process.exit(result.status || 1);
for (const name of ['pet.html', 'shared.html', 'pet.css', 'whale-points.tsv', 'pixel-whale.webp']) await cp(root + 'public/' + name, root + 'dist/' + name);
const modules = new Map();
async function moduleFor(name) {
  if (modules.has(name)) return;
  if (!/^[a-z-]+$/.test(name)) throw new Error('Unexpected native module dependency.');
  const source = await readFile(root + `src/core/${name}.ts`, 'utf8');
  const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
  modules.set(name, js);
  for (const m of js.matchAll(/require\("\.\/([a-z-]+)\.js"\)/g)) await moduleFor(m[1]);
}
await moduleFor('pet-native');
// JavaScriptCore has no structuredClone host API. Pet inputs are validated JSON.
const bundle = `/* Generated from pet/src/core. Run npm --prefix pet run sync. */\n(function(global){\n'use strict';\nif(!global.structuredClone)global.structuredClone=value=>JSON.parse(JSON.stringify(value));\nconst factories={},cache={};\n`
  + [...modules].map(([id, js]) => `factories[${JSON.stringify(id)}]=function(exports,require){\n${js}\n};\n`).join('')
  + `function load(id){id=id.replace(/^\\.\\//,'').replace(/\\.js$/,'');if(cache[id])return cache[id];if(!factories[id])throw Error('Missing core module');const e=cache[id]={};factories[id](e,load);return e;}\nglobal.PetNative=load('pet-native').PetNative;\n})(globalThis);\n`;
await writeFile(root + 'dist/pet-native.js', bundle);
// Committed native copies are generated from src/core and public/. `--check`
// regenerates them in memory and fails on any byte difference, so a green
// `npm run check` cannot sit beside a stale embedded TUI/Apple/Android core.
const check = process.argv.includes('--check');
const generated = [];
if (check || process.argv.includes('--tui')) {
  const destination = root + '../crates/tui/src/tui/pet_watch/';
  generated.push([destination + 'pet-native.js', Buffer.from(bundle)],
    [destination + 'shared.html', await readFile(root + 'public/shared.html')],
    [destination + 'pixel-whale.webp', await readFile(root + 'public/pixel-whale.webp')]);
}
if (process.argv.includes('--study')) {
  const study = root + 'world/';
  await mkdir(study, { recursive: true });
  for (const name of ['pet.html', 'pet.css', 'whale-points.tsv', 'pixel-whale.webp', 'core', 'ui/pet.js', 'ui/storage.js']) {
    await mkdir(study + name.split('/').slice(0, -1).join('/'), { recursive: true });
    await cp(root + 'dist/' + name, study + name, { recursive: true });
  }
  // The original conformance viewer must not retain a stale browser core.
  await mkdir(study + '../dist', { recursive: true });
  await writeFile(study + '../dist/PetSim.js', "export * from '../world/core/pet-sim.js';\n");
  const renderer = await readFile(study + '../web.ts', 'utf8');
  await writeFile(study + '../dist/web.js', ts.transpileModule(renderer, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
  }).outputText.replace("'./PetSim.ts'", "'./PetSim.js'"));
}
if (check || process.argv.includes('--apple')) {
  const { petDemoEvents } = await import('../dist/core/pet-demo.js');
  const { compilePetTelemetry, encodePetJSONL } = await import('../dist/core/pet-telemetry.js');
  generated.push([root + 'ios/Resources/pet-native.js', Buffer.from(bundle)],
    [root + 'ios/Resources/demo.jsonl', Buffer.from(encodePetJSONL(compilePetTelemetry(petDemoEvents(), 80_000)))]);
}
if (check) {
  const stale = [];
  for (const [path, bytes] of generated) {
    const current = await readFile(path).catch(error => { if (error.code === 'ENOENT') return undefined; throw error; });
    if (!current || !bytes.equals(current)) stale.push(path.slice(root.length));
  }
  if (stale.length) {
    console.error(`Generated native artifacts are stale: ${stale.join(', ')}. Run npm --prefix pet run sync and commit them.`);
    process.exit(1);
  }
} else {
  for (const [path, bytes] of generated) { await mkdir(dirname(path), { recursive: true }); await writeFile(path, bytes); }
}
console.log(`Built pet viewer and native core (${modules.size} shared modules), entirely local${check ? '; generated native artifacts match' : ''}.`);
