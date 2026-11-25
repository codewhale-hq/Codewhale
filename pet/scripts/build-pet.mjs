#!/usr/bin/env node
/** Local TypeScript build plus deterministic embedded-runtime packaging. */
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname } from 'node:path';
import { spawnSync } from 'node:child_process';
import ts from 'typescript';
const root = fileURLToPath(new URL('../', import.meta.url));
const result = spawnSync(process.execPath, [root + 'node_modules/typescript/bin/tsc', '-p', root + 'tsconfig.json'], { stdio: 'inherit' });
if (result.status !== 0) process.exit(result.status || 1);
const modules = new Map();
async function moduleFor(name) {
  if (modules.has(name)) return;
  if (!/^[a-z-]+$/.test(name)) throw new Error('Unexpected native module dependency.');
  const source = await readFile(root + `src/core/${name}.ts`, 'utf8');
  const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
  modules.set(name, js);
  for (const m of js.matchAll(/require\("\.\/([a-z-]+)\.js"\)/g)) await moduleFor(m[1]);
}
for (const entry of ['pet-engine', 'dot-field']) await moduleFor(entry);
// The embedded QuickJS host has no structuredClone API. Pet inputs are validated JSON.
const bundle = `/* Generated from pet/src/core. Run npm --prefix pet run sync. */\n(function(global){\n'use strict';\nif(!global.structuredClone)global.structuredClone=value=>JSON.parse(JSON.stringify(value));\nconst factories={},cache={};\n`
  + [...modules].map(([id, js]) => `factories[${JSON.stringify(id)}]=function(exports,require){\n${js}\n};\n`).join('')
  + `function load(id){id=id.replace(/^\\.\\//,'').replace(/\\.js$/,'');if(cache[id])return cache[id];if(!factories[id])throw Error('Missing core module');const e=cache[id]={};factories[id](e,load);return e;}\nglobal.PetDotField=load('dot-field');global.PetEngineTelemetry=load('pet-engine').PetEngineTelemetry;\n})(globalThis);\n`;
await writeFile(root + 'dist/pet-native.js', bundle);
// The committed TUI copy is generated from src/core. `--check` regenerates it
// in memory and fails on any byte difference, so a green `npm run check`
// cannot sit beside a stale embedded TUI core.
const check = process.argv.includes('--check');
const generated = [];
if (check || process.argv.includes('--tui')) {
  const destination = root + '../crates/tui/src/tui/pet_watch/';
  generated.push([destination + 'pet-native.js', Buffer.from(bundle)]);
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
console.log(`Built pet native core (${modules.size} shared modules), entirely local${check ? '; generated native artifacts match' : ''}.`);
