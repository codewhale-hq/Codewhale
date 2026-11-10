import { readFile, writeFile, readdir, unlink } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { controls } from './controls.spec.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const appIndex = process.argv.indexOf('--app');
const app = path.resolve(appIndex < 0 ? path.join(here, '../../codewhale-app') : process.argv[appIndex + 1]);
const check = process.argv.includes('--check');
const sourceRoot = path.join(app, 'packages/ui/src/components');
const sources = new Map();
const payloadBehaviorSources = {};
const digest = text => createHash('sha256').update(text).digest('hex');
async function source(file) {
  if (sources.has(file)) return sources.get(file);
  const text = await readFile(path.join(sourceRoot, file + '.module.css'), 'utf8');
  const rules = new Map();
  for (const match of text.replace(/\/\*[\s\S]*?\*\//g, '').matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const properties = Object.fromEntries(match[2].split(';').filter(x => x.includes(':')).map(x => { const i = x.indexOf(':'); return [x.slice(0, i).trim(), x.slice(i + 1).trim()]; }));
    for (const selector of match[1].trim().split(/,\s*(?![^()]*\))/)) rules.set(selector.trim(), { ...rules.get(selector.trim()), ...properties });
  }
  const result = { text, rules, sha256: digest(text) };
  sources.set(file, result);
  return result;
}
async function style(file, selectors, stack = []) {
  let result = {};
  for (const selector of selectors) {
    const split = selector.indexOf(':.');
    const targetFile = split < 0 ? file : selector.slice(0, split);
    const targetSelector = split < 0 ? selector : selector.slice(split + 1);
    const key = targetFile + ':' + targetSelector;
    if (stack.includes(key)) throw Error('CSS composition cycle: ' + key);
    const css = await source(targetFile);
    const rule = css.rules.get(targetSelector);
    if (!rule) throw Error('Missing source selector: ' + key);
    if (rule.composes) {
      const match = rule.composes.match(/^(\w+) from ['"](.+)\.module\.css['"]$/);
      if (!match) throw Error('Unsupported CSS composition: ' + rule.composes);
      result = { ...result, ...await style(path.normalize(path.join(path.dirname(targetFile), match[2])), ['.' + match[1]], [...stack, key]) };
    }
    result = { ...result, ...rule };
  }
  delete result.composes;
  return result;
}
const iconFor = { '✓': 'check', '⌄': 'chevron-down', '!': 'warning', i: 'info' };
async function compile(tree) {
  const styles = { ...await style(tree.file, tree.selectors), ...tree.override };
  if (tree.behavior === 'textarea-default') {
    const tsx = await readFile(path.join(sourceRoot, 'TextArea.tsx'), 'utf8');
    const rows = Number(tsx.match(/rows = (\d+)/)?.[1]);
    if (!rows) throw Error('Cannot resolve TextArea default rows');
    const line = Number.parseFloat(styles['line-height']);
    const padding = Number.parseFloat(styles.padding);
    const border = Number.parseFloat(styles.border);
    tree = { ...tree, height: rows * line + 2 * padding + 2 * border };
    payloadBehaviorSources['TextArea.tsx'] = digest(tsx);
  }
  if (tree.gap) styles.gap = tree.gap;
  const children = await Promise.all(tree.children.map(compile));
  if (tree.icon) children.push({ name: tree.name + ' icon', styles: {}, children: [], svg: await readFile(path.join(here, '../icons', tree.icon + '.svg'), 'utf8') });
  const icon = tree.icon || (tree.text && iconFor[tree.text]);
  const keptStyles = Object.fromEntries(Object.entries(styles).filter(([key]) => ['border-top','border-bottom','background','background-color','color','border','border-color','border-radius','padding','padding-top','padding-right','padding-bottom','padding-left','min-width','min-height','width','height','gap','flex-direction','align-items','justify-content','overflow','outline','font-family','font-size','line-height','font-weight','text-align','flex'].includes(key)));
  const { file, selectors, override, ...shape } = tree;
  return { ...shape, styles: keptStyles, children, ...(icon && !tree.icon ? { icon, svg: await readFile(path.join(here, '../icons', icon + '.svg'), 'utf8') } : {}) };
}
const payload = { version: JSON.parse(await readFile(path.join(here, '../tokens.json'), 'utf8')).version, controls: [] };
for (const family of controls) payload.controls.push({ name: family.name, variants: await Promise.all(family.variants.map(async variant => ({ state: variant.state, tree: await compile(variant.tree) }))) });
payload.sources = Object.fromEntries([...sources].sort(([a], [b]) => a.localeCompare(b)).map(([file, data]) => [file + '.module.css', data.sha256]));
const tokens = await readFile(path.join(here, '../tokens.css'), 'utf8');
payload.cssVariables = Object.fromEntries([...tokens.matchAll(/(--[\w-]+):\s*([^;{}]+);/g)].map(m => [m[1], m[2]]).filter(([,value]) => /^-?[\d.]+(?:px)?$/.test(value) || value.startsWith('var(') || value.startsWith('color-mix(')));
const variables = JSON.parse(await readFile(path.join(here, 'variables.json'), 'utf8'));
payload.variables = variables.collections.flatMap(c => c.variables.map(v => ({ collection: c.name, name: v.name, web: v.web, type: v.type })));
payload.typeRoles = variables.textStyles;
const template = await readFile(path.join(here, 'controls.template.js'), 'utf8');
const output = 'const CW_CONTROLS = ' + JSON.stringify(payload) + ';\n' + template;
const manifest = JSON.stringify({ version: payload.version, appSource: 'codewhale-app/packages/ui/src/components', controls: payload.controls.map(c => ({ name: c.name, states: c.variants.map(v => v.state) })), sourceDigests: { ...payload.sources, ...payloadBehaviorSources } }, null, 2) + '\n';
for (const [name, content] of [['controls.js', output], ['controls.manifest.json', manifest]]) {
  const target = path.join(here, name);
  if (check) { if (await readFile(target, 'utf8') !== content) throw Error(name + ' is stale'); }
  else await writeFile(target, content);
}
let batch = [], batchIndex = 1;
async function emitBatch() {
 const content = 'const CW_CONTROLS = ' + JSON.stringify({ ...payload, sources: undefined, controls: batch }) + ';\n' + template;
 if (content.length > 49900) throw Error('One control exceeds the Figma call size; reduce generated duplication.');
 const target = path.join(here, 'controls-' + batchIndex++ + '.js');
 if (check) { if (await readFile(target, 'utf8') !== content) throw Error(target + ' is stale'); } else await writeFile(target, content);
 batch = [];
}
for (const family of payload.controls) {
 const candidate = 'const CW_CONTROLS = ' + JSON.stringify({ ...payload, sources: undefined, controls: [...batch, family] }) + ';\n' + template;
 if (candidate.length > 45000 && batch.length) await emitBatch();
 batch.push(family);
}
if (batch.length) await emitBatch();
if (!check) for (const name of await readdir(here)) {
 const match = name.match(/^controls-(\d+)\.js$/);
 if (match && Number(match[1]) >= batchIndex) await unlink(path.join(here, name));
}
console.log(JSON.stringify({ mode: check ? 'check' : 'generate', families: payload.controls.length, variants: payload.controls.reduce((n, c) => n + c.variants.length, 0), cssSources: sources.size, tokenVersion: payload.version, canvasExecuted: false }));
