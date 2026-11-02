// Real Node/Bun host and reviewed source closure; fake core transport.
// Rust receipt/admission/membership/install qualification is a separate gate.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { activate, startHost, FIXTURES, IS_BUN } from './harness.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const hash = (value) => createHash('sha256').update(value).digest('hex')

function temporary(t) {
  const root = mkdtempSync(join(tmpdir(), 'cw-composition-'))
  t.after(() => rmSync(root, { recursive: true, force: true }))
  return root
}

test('canonical Native composition adopts existing contributions and withdraws their exact handles', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const data_dir = temporary(t)
  const entry = join(FIXTURES, 'native-dsh-composition', 'native', 'index.mjs')
  const { ref, result } = await activate(host, 'native-dsh-composition', entry, { data_dir })
  assert.equal(result.status, 'ok', result.diagnostic)
  const admitted = host.registry.filter(item => item.op === 'register')
  assert.deepEqual(new Set(admitted.map(item => item.kind)), new Set(['tool', 'command', 'prompt_section', 'skill_root']))
  assert.equal(admitted.find(item => item.kind === 'prompt_section').spec.description, 'hello from upstream')
  assert.equal(admitted.find(item => item.kind === 'skill_root').spec.name, 'source/skills')
  const tool = admitted.find(item => item.kind === 'tool')
  const answer = await host.call('tool/call', { handle: tool.handle, input: {}, call_id: 'composition-call', deadline_ms: 5000, workspace: data_dir })
  assert.deepEqual(answer.structured, { text: 'hello from upstream', count: null })
  assert.deepEqual(await host.call('ext/deactivate', { owner: ref }), { disposed: true, leaked: [] })
  for (const item of admitted) assert.ok(host.registry.some(row => row.op === 'unregister' && row.handle === item.handle))
})

function counterComposition(t, id) {
  const root = temporary(t)
  mkdirSync(join(root, 'source'))
  const source = `export const inject = ['prompt']; export function apply(ctx, config) { ctx.prompt.registerSection({id:${JSON.stringify(id)},text:String(config.count)}) }`
  writeFileSync(join(root, 'source', 'row.mjs'), source)
  const layer = `- insert:\n  - id: count\n    name: ./row.mjs\n    config:\n      count: !!js '(globalThis.__CW_COMPOSITION_COUNT = (globalThis.__CW_COMPOSITION_COUNT ?? 0) + 1)'\n`
  const spec = { version: 1, layers: [{ path: 'base.yml', sha256: hash(layer), source: layer }], modules: [{ name: './row.mjs', path: 'row.mjs', sha256: hash(source) }], files: { 'row.mjs': hash(source) } }
  writeFileSync(join(root, 'composition.json'), JSON.stringify(spec))
  const entry = join(root, 'index.mjs')
  writeFileSync(entry, `import {mountReviewedComposition} from '@codewhale/dsh-composition'; import spec from './composition.json' with {type:'json'}; export async function apply(ctx) {await mountReviewedComposition(ctx,new URL('./source/',import.meta.url).href,spec)}`)
  return entry
}

test('two compositions share exactly one Loader and one teardown keeps the other owner live', async (t) => {
  const host = await startHost()
  t.after(() => host.stop())
  const a = await activate(host, 'composition-a', counterComposition(t, 'note-a'))
  const b = await activate(host, 'composition-b', counterComposition(t, 'note-b'))
  assert.equal(a.result.status, 'ok', a.result.diagnostic)
  assert.equal(b.result.status, 'ok', b.result.diagnostic)
  const notes = host.registry.filter(item => item.op === 'register' && item.kind === 'prompt_section')
  assert.deepEqual(notes.map(item => item.spec.description), ['1', '2'], 'duplicate global Loader observers evaluated a configuration twice')
  assert.deepEqual(await host.call('ext/deactivate', { owner: a.ref }), { disposed: true, leaked: [] })
  assert.ok(!host.registry.some(item => item.op === 'unregister' && item.handle === notes[1].handle))
  const c = await activate(host, 'composition-c', counterComposition(t, 'note-c'))
  assert.equal(c.result.status, 'ok', c.result.diagnostic)
  assert.equal(host.registry.filter(item => item.op === 'register' && item.kind === 'prompt_section').at(-1).spec.description, '3')
})

test('trusted offline reviewer preserves expressions and never imports row modules', (t) => {
  const root = temporary(t)
  const sentinel = join(root, 'expression-executed')
  const layer = JSON.stringify([{ insert: [{ id: 'unexecuted', name: './row.mjs', config: { __jsExpr: `process.getBuiltinModule('fs').writeFileSync(${JSON.stringify(sentinel)},'unsafe')` } }] }])
  const spec = { version: 1, layers: [{ path: 'base.json', sha256: hash(layer), source: layer }], modules: [{ name: './row.mjs', path: 'row.mjs', sha256: '0'.repeat(64) }], files: { 'row.mjs': '0'.repeat(64) } }
  const result = spawnSync(process.execPath, [join(here, '..', 'dist', 'dsh-composition-review.mjs')], { input: JSON.stringify(spec), encoding: 'utf8', timeout: 5000, maxBuffer: 8 * 1024 * 1024 })
  assert.equal(result.status, 0, result.stderr)
  const review = JSON.parse(result.stdout)
  assert.deepEqual(review.entries[0].config, JSON.parse(layer)[0].insert[0].config)
  assert.equal(existsSync(sentinel), false)
  assert.equal(existsSync(join(root, 'row.mjs')), false, 'review did not need any row module to exist')
})

function moduleClosureComposition(t, rowSource, files, extraRows = {}) {
  const root=temporary(t)
  mkdirSync(join(root,'source'))
  const rows={'row.mjs':rowSource,...extraRows}
  for (const [name,source] of Object.entries({...rows,...files})) writeFileSync(join(root,'source',name),source)
  const layer=JSON.stringify([{insert:Object.keys(rows).map((name,index)=>({id:`row-${index}`,name:`./${name}`,config:{}}))}])
  const closure=Object.fromEntries(Object.entries({...rows,...files}).map(([name,source])=>[name,hash(source)]))
  const spec={version:1,layers:[{path:'base.json',sha256:hash(layer),source:layer}],modules:Object.entries(rows).map(([name,source])=>({name:`./${name}`,path:name,sha256:hash(source)})),files:closure}
  writeFileSync(join(root,'composition.json'),JSON.stringify(spec))
  const entry=join(root,'index.mjs')
  writeFileSync(entry,`import {mountReviewedComposition} from '@codewhale/dsh-composition';import spec from './composition.json' with {type:'json'};export async function apply(ctx){await mountReviewedComposition(ctx,new URL('./source/',import.meta.url).href,spec)}`)
  return {root,entry}
}

test('a transitive reviewed relative helper executes but an undeclared helper never does',async t=>{
  const host=await startHost();t.after(()=>host.stop())
  const good=moduleClosureComposition(t,"import {text} from './helper.mjs';export const inject=['prompt'];export function apply(ctx){ctx.prompt.registerSection({id:'nested',text})}",{'helper.mjs':"export const text='reviewed helper'"})
  const mounted=await activate(host,'closure-good',good.entry)
  assert.equal(mounted.result.status,'ok',mounted.result.diagnostic)
  assert.equal(host.registry.find(row=>row.op==='register' && row.kind==='prompt_section').spec.description,'reviewed helper')
  const bad=moduleClosureComposition(t,"import './rogue.mjs';export function apply(){}",{})
  const sentinel=join(bad.root,'rogue-executed')
  writeFileSync(join(bad.root,'source','rogue.mjs'),`import {writeFileSync} from 'node:fs';writeFileSync(${JSON.stringify(sentinel)},'bad')`)
  const refused=await activate(host,'closure-undeclared',bad.entry)
  assert.equal(refused.result.status,'failed')
  assert.equal(existsSync(sentinel),false)
  assert.deepEqual(await host.call('ext/deactivate',{owner:mounted.ref}),{disposed:true,leaked:[]})
})

test('changed transitive module bytes and ambient bare dependencies refuse before execution',async t=>{
  const host=await startHost();t.after(()=>host.stop())
  const changed=moduleClosureComposition(t,"import './helper.mjs';export function apply(){}",{'helper.mjs':"export const original=true"})
  const sentinel=join(changed.root,'changed-executed')
  writeFileSync(join(changed.root,'source','helper.mjs'),`import {writeFileSync} from 'node:fs';writeFileSync(${JSON.stringify(sentinel)},'bad')`)
  const refused=await activate(host,'closure-changed',changed.entry)
  assert.equal(refused.result.status,'failed')
  assert.equal(existsSync(sentinel),false)
  const ambient=moduleClosureComposition(t,"import 'js-yaml';export function apply(){}",{})
  const unavailable=await activate(host,'closure-ambient',ambient.entry)
  assert.equal(unavailable.result.status,'failed')
})


test('reviewed composition refuses a row creation failure instead of acknowledging an empty tree', async t => {
  const host=await startHost();t.after(()=>host.stop())
  const entry=counterComposition(t,'creation-probe')
  const source=String.raw`export const inject=['prompt'];export function apply(ctx){throw new Error('intentional row startup refusal')}`
  // Full rows stay reviewed; the failure occurs inside activation, not the offline reviewer.
  const root=dirname(entry)
  writeFileSync(join(root,'source','row.mjs'),source)
  const layer=JSON.stringify([{insert:[{id:'must-start',name:'./row.mjs',config:{}}]}])
  writeFileSync(join(root,'composition.json'),JSON.stringify({version:1,layers:[{path:'base.json',sha256:hash(layer),source:layer}],modules:[{name:'./row.mjs',path:'row.mjs',sha256:hash(source)}],files:{'row.mjs':hash(source)}}))
  const result=await activate(host,'startup-refusal',entry)
  assert.equal(result.result.status,'failed')
  assert.equal(host.registry.filter(row=>row.op==='register').length,0)
})


test('reviewed computed and URL imports retain query identity and exact JSON data', async t => {
  const host=await startHost();t.after(()=>host.stop())
  const row=`export const inject=['prompt'];export async function apply(ctx){
    const name='./helper.mjs';const first=await import(name);
    const url=new URL('./helper.mjs?variant=two',import.meta.url);
    const queried=await import(url);const again=await import(String(url));
    const data=await import('./data.json',{with:{type:'json'}});
    ctx.prompt.registerSection({id:'dynamic',text:JSON.stringify({text:first.text,queryDistinct:first!==queried,queryCached:queried===again,protoData:Object.hasOwn(data.default,'__proto__'),value:data.default.__proto__.answer})});
  }`
  const composition=moduleClosureComposition(t,row,{'helper.mjs':"export const text='computed reviewed helper'",'data.json':'{"__proto__":{"answer":42}}'})
  const mounted=await activate(host,'closure-computed',composition.entry)
  if(IS_BUN){
    assert.equal(mounted.result.status,'failed')
    assert.match(mounted.result.diagnostic,/Bun does not preserve reviewed module query or fragment identity/)
    assert.match(mounted.result.diagnostic,/runtime = "node"/)
    assert.equal(host.registry.filter(row=>row.op==='register').length,0)
    return
  }
  assert.equal(mounted.result.status,'ok',mounted.result.diagnostic)
  assert.deepEqual(JSON.parse(host.registry.find(row=>row.op==='register' && row.kind==='prompt_section').spec.description),{text:'computed reviewed helper',queryDistinct:true,queryCached:true,protoData:true,value:42})
  assert.deepEqual(await host.call('ext/deactivate',{owner:mounted.ref}),{disposed:true,leaked:[]})
})

test('unreviewed computed imports refuse before module side effects', async t => {
  const host=await startHost();t.after(()=>host.stop())
  const composition=moduleClosureComposition(t,"export async function apply(){const name='./rogue.mjs';await import(name)}",{})
  const sentinel=join(composition.root,'computed-rogue-executed')
  writeFileSync(join(composition.root,'source','rogue.mjs'),`import {writeFileSync} from 'node:fs';writeFileSync(${JSON.stringify(sentinel)},'bad')`)
  const mounted=await activate(host,'closure-computed-rogue',composition.entry)
  assert.equal(mounted.result.status,'failed')
  assert.equal(existsSync(sentinel),false)
  assert.equal(host.registry.filter(row=>row.op==='register').length,0)
})

test('separate reviewed rows retain the same transitive module instance', async t => {
  const host=await startHost();t.after(()=>host.stop())
  const row=id=>`import {next} from './helper.mjs';export const inject=['prompt'];export function apply(ctx){ctx.prompt.registerSection({id:${JSON.stringify(id)},text:String(next())})}`
  const composition=moduleClosureComposition(t,row('first'),{'helper.mjs':'let count=0;export function next(){return ++count}'},{'second.mjs':row('second')})
  const mounted=await activate(host,'closure-shared-instance',composition.entry)
  assert.equal(mounted.result.status,'ok',mounted.result.diagnostic)
  const registrations=host.registry.filter(row=>row.op==='register' && row.kind==='prompt_section')
  assert.deepEqual(registrations.map(row=>row.spec.description),['1','2'])
  assert.deepEqual(await host.call('ext/deactivate',{owner:mounted.ref}),{disposed:true,leaked:[]})
  for(const row of registrations)assert.ok(host.registry.some(item=>item.op==='unregister' && item.handle===row.handle))
})


test('reviewed computed URL imports and JSON preserve data on both runtimes', async t => {
  const host=await startHost();t.after(()=>host.stop())
  const row=`export const inject=['prompt'];export async function apply(ctx){
    const target=new URL('./helper.mjs',import.meta.url);const helper=await import(target);
    const again=await import(String(target));const data=await import('./data.json',{with:{type:'json'}});
    ctx.prompt.registerSection({id:'computed-url',text:JSON.stringify({text:helper.text,cached:helper===again,protoData:Object.hasOwn(data.default,'__proto__'),value:data.default.__proto__.answer})});
  }`
  const composition=moduleClosureComposition(t,row,{'helper.mjs':"export const text='reviewed URL helper'",'data.json':'{"__proto__":{"answer":42}}'})
  const mounted=await activate(host,'closure-computed-url',composition.entry)
  assert.equal(mounted.result.status,'ok',mounted.result.diagnostic)
  assert.deepEqual(JSON.parse(host.registry.find(row=>row.op==='register' && row.kind==='prompt_section').spec.description),{text:'reviewed URL helper',cached:true,protoData:true,value:42})
  assert.deepEqual(await host.call('ext/deactivate',{owner:mounted.ref}),{disposed:true,leaked:[]})
})
