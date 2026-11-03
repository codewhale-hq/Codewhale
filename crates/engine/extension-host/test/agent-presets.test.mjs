// Source-focused Node host/fake Core acceptance. Rust admission/selection is separate.
import {test} from 'node:test'
import assert from 'node:assert/strict'
import {createHash} from 'node:crypto'
import {mkdtempSync,mkdirSync,writeFileSync,readFileSync,rmSync} from 'node:fs'
import {join,dirname} from 'node:path'
import {tmpdir} from 'node:os'
import {fileURLToPath} from 'node:url'
import {reviewAgentPresets,containedPackageModule} from '../dist/agent-presets.mjs'
import {spawnSync} from 'node:child_process'
import {activate,startHost} from './harness.mjs'
const hash=s=>createHash('sha256').update(s).digest('hex')
function request(config,contents,extraDirs=[]) {
  const layer=JSON.stringify([{insert:[{id:'raw-roster',name:'@deepseek-ai/dsh-agent-presets',config}]}])
  const directories=new Set(extraDirs)
  for(const path of Object.keys(contents)) { let dir=dirname(path);while(dir!=='.'){directories.add(dir);dir=dirname(dir)} }
  const files=Object.fromEntries(Object.entries(contents).map(([path,text])=>[path,hash(text)]))
  return {kind:'agent-presets',composition:{version:1,layers:[{path:'bundle.json',sha256:hash(layer),source:layer}],modules:[],files},directories:[...directories],documents:Object.entries(contents).filter(([path])=>/\/(?:package\.json|preset\.yml|agent\.cordis\.yml)$/.test(path)).map(([path,source])=>({path,source,sha256:hash(source)}))}
}
const cfg={includeShippedRoot:false,includeUserRoot:false,roots:[{path:'presets',trust:'user'}],default:'a'}
const empty='[]\n'

test('raw discovery preserves first-root-wins, metadata order, broken rows and ignored metadata trust',async()=>{
  const input=request({...cfg,roots:[{path:'system',trust:'system'},{path:'user',trust:'user'}],default:'a'},{
    'system/a/agent.cordis.yml':empty,'system/a/preset.yml':'name: Alpha\norder: 2\nid: spoof\ntrust: admin\n',
    'system/b/agent.cordis.yml':empty,'system/b/preset.yml':'name: Beta\norder: 1\n',
    'user/a/agent.cordis.yml':empty,'user/a/preset.yml':'name: Shadowed\n',
    'user/c/agent.cordis.yml':empty,'user/c/preset.yml':'[malformed\n',
  },['user/missing','user/INVALID'])
  const review=await reviewAgentPresets(input)
  assert.deepEqual(review.catalog.presets.map(p=>p.id),['b','a','c','missing'])
  assert.equal(review.catalog.presets[1].name,'Alpha');assert.equal(review.catalog.presets[1].trust,'system')
  assert.equal(review.catalog.presets[2].name,undefined);assert.match(review.catalog.presets[3].broken,/missing/)
  assert.deepEqual(review.presets.map(p=>p.metadata.id),['b','a','c'])
})

test('confined bare lookup honors exact ESM export and refuses ambient/escape/require-only exports',async()=>{
  const manifests=new Map([['node_modules/@test/profile/package.json',JSON.stringify({name:'@test/profile',type:'module',exports:{'.':{require:'./bad.cjs',import:'./main.mjs'},'./review':'./review.mjs'}})]])
  const files={'node_modules/@test/profile/main.mjs':hash('main'),'node_modules/@test/profile/review.mjs':hash('review')}
  assert.equal(containedPackageModule('@test/profile',manifests,files).path,'node_modules/@test/profile/main.mjs')
  assert.equal(containedPackageModule('@test/profile/review',manifests,files).path,'node_modules/@test/profile/review.mjs')
  for(const name of ['js-yaml','@test/profile/../../outside','file:///outside']) assert.equal(containedPackageModule(name,manifests,files),undefined)
  const blocked=new Map([['node_modules/@test/profile/package.json',JSON.stringify({name:'@test/profile',type:'module',exports:{node:null,import:'./main.mjs'}})]])
  assert.equal(containedPackageModule('@test/profile',blocked,files),undefined,'a blocked matched condition never falls through')
  const input=request(cfg,{'presets/a/agent.cordis.yml':'- name: "@test/profile"\n','node_modules/@test/profile/package.json':manifests.get('node_modules/@test/profile/package.json'),'node_modules/@test/profile/main.mjs':'export function apply(){}'})
  const review=await reviewAgentPresets(input)
  assert.equal(review.presets[0].composition.modules[0].path,'node_modules/@test/profile/main.mjs')
})

test('raw roots and bytes stay admitted; disabled groups and conditional missing rows are not guessed',async()=>{
  const input=request(cfg,{'presets/a/agent.cordis.yml':"- name: cordis:group\n  group: true\n  disabled: true\n  config:\n  - name: absent\n- name: absent-conditional\n  disabled: !!js 'true'\n"})
  const review=await reviewAgentPresets(input)
  assert.equal(review.presets.length,1);assert.equal(review.presets[0].composition.modules.length,0)
  const changed=structuredClone(input);changed.documents[0].source+=' # tampered'
  await assert.rejects(reviewAgentPresets(changed),/document changed/)
  for(const config of [{...cfg,roots:[{path:'../outside',trust:'user'}]},{...cfg,includeUserRoot:true},{...cfg,includeShippedRoot:true}]) {
    await assert.rejects(reviewAgentPresets(request(config,{'presets/a/agent.cordis.yml':empty})),/admitted|inside/)
  }
})

test('cyclic/oversized preset rows remain broken and cannot recurse discovery or replace Core prompt',async()=>{
  const input=request(cfg,{'presets/a/agent.cordis.yml':empty,
    'presets/cycle/agent.cordis.yml':'&rows\n- name: cordis:group\n  group: true\n  config: *rows\n',
    'presets/minimal/agent.cordis.yml':'- name: "@deepseek-ai/dsh-persona"\n  config: {complete: true}\n',
    'node_modules/@deepseek-ai/dsh-persona/package.json':JSON.stringify({name:'@deepseek-ai/dsh-persona',type:'module',exports:'./index.mjs'}),
    'node_modules/@deepseek-ai/dsh-persona/index.mjs':'export function apply(){}',
  })
  const review=await reviewAgentPresets(input)
  assert.deepEqual(review.presets.map(p=>p.metadata.id),['a'])
  assert.match(review.catalog.presets.find(p=>p.id==='cycle').broken,/cannot be read/)
  assert.match(review.catalog.presets.find(p=>p.id==='minimal').broken,/Core prompt/)
  const additive=request(cfg,{'presets/a/agent.cordis.yml':empty,'presets/persona/agent.cordis.yml':'- name: "@deepseek-ai/dsh-persona"\n  config: {prefix: additive}\n','node_modules/@deepseek-ai/dsh-persona/package.json':JSON.stringify({name:'@deepseek-ai/dsh-persona',type:'module',exports:'./index.mjs'}),'node_modules/@deepseek-ai/dsh-persona/index.mjs':'export function apply(){}'})
  const bounded=await reviewAgentPresets(additive)
  assert.equal(bounded.catalog.presets.find(p=>p.id==='persona').broken,undefined)
  assert.deepEqual(bounded.presets.map(p=>p.metadata.id),['a','persona'])
  await assert.rejects(reviewAgentPresets({...input,composition:{...input.composition,layers:input.composition.layers.map(l=>{const source=l.source.replace('"default":"a"','"default":"minimal"');return {...l,source,sha256:hash(source)}})}}),/default preset is missing or broken/)
})

function authoredRow(value) {
  return `export const inject=['tools','commands','prompt','skills','agentPresets'];
export function apply(ctx){
const value=${JSON.stringify(value)}+':'+ctx.agentPresets.composedPreset();
ctx.tools.register({name:'preset_echo',description:'exact selected preset',parameters:{type:'object',properties:{}},execute:()=>value});
ctx.commands.register({name:'preset-echo',description:'exact selected preset',handler:()=>value});
ctx.prompt.registerSection({id:'selected',text:value});
ctx.skills.registerRoot({path:'source/skills/${value}'});
ctx.on('tools/pre-execute',()=>({kind:'annotate',text:value}));
}`
}
function installedEntries(t,review,contents) {
  const root=mkdtempSync(join(tmpdir(),'cw-raw-presets-'));t.after(()=>rmSync(root,{recursive:true,force:true}))
  for(const [path,source] of Object.entries(contents)){const target=join(root,'source',path);mkdirSync(dirname(target),{recursive:true});writeFileSync(target,source)}
  mkdirSync(join(root,'native/presets'),{recursive:true})
  return review.presets.map(selected=>{
    const id=selected.metadata.id
    const entry=join(root,'native/presets',`${id}.mjs`)
    writeFileSync(join(root,'native/presets',`${id}.json`),JSON.stringify({catalog:review.catalog,selected,composition:review.top}))
    const source=`import {mountReviewedPreset} from '@codewhale/dsh-composition';import data from './${id}.json' with {type:'json'};export async function apply(ctx){await mountReviewedPreset(ctx,new URL('../../source/',import.meta.url).href,data.composition,data.catalog,data.selected)}`
    writeFileSync(entry,source);return {path:entry,sha256:hash(source)}
  })
}

test('real raw roster mounts all five registries under one exact selected entry and tears down only that sibling',async t=>{
  const host=await startHost();t.after(()=>host.stop())
  const contents={
    'presets/a/agent.cordis.yml':'- name: ./row.mjs\n','presets/a/row.mjs':authoredRow('A'),
    'presets/b/agent.cordis.yml':'- name: "@demo/preset-row"\n',
    'node_modules/@demo/preset-row/package.json':JSON.stringify({name:'@demo/preset-row',type:'module',exports:'./index.mjs'}),
    'node_modules/@demo/preset-row/index.mjs':authoredRow('B'),
  }
  const input=request(cfg,contents),review=await reviewAgentPresets(input);review.top=input.composition
  const [a,b]=installedEntries(t,review,contents)
  const first=await activate(host,'raw-presets',a.path,{scope:a})
  assert.equal(first.result.status,'ok',first.result.diagnostic+' '+JSON.stringify(host.logs)+' '+host.stderr)
  const second=await host.call('ext/activate',{owner:first.ref,plugin_name:'raw-presets',entry:b,scope:b,config:{}})
  assert.equal(second.status,'ok',second.diagnostic)
  const registrations=host.registry.filter(r=>r.op==='register')
  for(const scope of [a,b])assert.deepEqual(new Set(registrations.filter(r=>r.scope.path===scope.path).map(r=>r.kind)),new Set(['tool','command','hook','prompt_section','skill_root']))
  const handle=(scope,kind)=>registrations.find(r=>r.scope.path===scope.path && r.kind===kind).handle
  const call=scope=>host.call('tool/call',{handle:handle(scope,'tool'),input:{},call_id:scope.path,deadline_ms:5000})
  assert.match(JSON.stringify(await call(a)),/A:a/);assert.match(JSON.stringify(await call(b)),/B:b/)
  assert.deepEqual(await host.call('command/run',{handle:handle(b,'command'),command_id:'preset-command',raw_input:'',deadline_ms:5000}),{kind:'success',text:'B:b'})
  assert.deepEqual(await host.call('hook/evaluate',{handle:handle(b,'hook'),event:'tools/pre-execute',deadline_ms:5000,payload:{name:'read',call_id:'preset-hook',input:{},mode:'Agent',workspace:'/workspace',model:'fixture'}}),{kind:'annotate',text:'B:b'})
  assert.deepEqual(await host.call('ext/deactivate',{owner:first.ref,entry:a}),{disposed:true,leaked:[]})
  await assert.rejects(call(a),/not live/);assert.match(JSON.stringify(await call(b)),/B:b/)
  for(const row of registrations.filter(r=>r.scope.path===a.path)) assert.ok(host.registry.some(r=>r.op==='unregister' && r.handle===row.handle))
  for(const row of registrations.filter(r=>r.scope.path===b.path)) assert.ok(!host.registry.some(r=>r.op==='unregister' && r.handle===row.handle))
  assert.deepEqual(await host.call('ext/deactivate',{owner:first.ref}),{disposed:true,leaked:[]})
})


test('absent raw default stays visible and requires explicit selection before real host mount',async t=>{
  const noDefault={...cfg};delete noDefault.default
  const contents={'presets/a/agent.cordis.yml':'- name: ./row.mjs\n','presets/a/row.mjs':authoredRow('A'),'presets/b/agent.cordis.yml':empty}
  const input=request(noDefault,contents),review=await reviewAgentPresets(input);review.top=input.composition
  assert.equal(review.catalog.default,undefined)
  assert.ok(review.catalog.presets.every(row=>row.is_default===false))
  const [a]=installedEntries(t,review,contents)
  // Core stages a whole reviewed bundle before importing any of its entries.
  // Creating a sibling after an import also hits Bun's directory resolver cache.
  const root=dirname(a.path),bad=join(root,'unselected.mjs')
  const source="import {mountReviewedPreset} from '@codewhale/dsh-composition';import data from './a.json' with {type:'json'};export async function apply(ctx){await mountReviewedPreset(ctx,new URL('../../source/',import.meta.url).href,data.composition,data.catalog,undefined)}"
  writeFileSync(bad,source)
  const host=await startHost();t.after(()=>host.stop())
  const first=await activate(host,'absent-default',a.path,{scope:a})
  assert.equal(first.result.status,'ok',first.result.diagnostic+' '+host.stderr)
  const tool=host.registry.find(row=>row.op==='register' && row.kind==='tool')
  assert.match(JSON.stringify(await host.call('tool/call',{handle:tool.handle,input:{},call_id:'explicit',deadline_ms:5000})),/A:a/)
  // Unselected generated mount is an error, never a first-healthy fallback.
  const refused=await activate(host,'missing-selected',bad,{scope:{path:bad,sha256:hash(source)}})
  assert.equal(refused.result.status,'failed')
  assert.match(refused.result.diagnostic,/explicit selection is required/)
})


test('actual installed source counterparts mount default and explicitly selected no-default entries',async t=>{
  const host=await startHost();t.after(()=>host.stop())
  for(const suffix of ['', '-no-default']) {
    const root=new URL(`../../tests/fixtures/extension_host/raw-agent-presets${suffix}/`,import.meta.url)
    const catalog=JSON.parse(readFileSync(new URL('native/presets.json',root),'utf8'))
    assert.equal(catalog.default,suffix?undefined:'a')
    const row=catalog.presets.find(p=>p.id==='b'),entry=fileURLToPath(new URL(row.entry.path,root))
    assert.equal(hash(readFileSync(entry)),row.entry.sha256)
    const admitted=await activate(host,'counterpart'+suffix,entry,{scope:{path:entry,sha256:row.entry.sha256}})
    assert.equal(admitted.result.status,'ok',admitted.result.diagnostic+' '+host.stderr)
    const tool=host.registry.findLast(r=>r.op==='register' && r.kind==='tool')
    assert.match(JSON.stringify(await host.call('tool/call',{handle:tool.handle,input:{},call_id:'fixed'+suffix,deadline_ms:5000})),/B:b/)
    assert.deepEqual(await host.call('ext/deactivate',{owner:admitted.ref}),{disposed:true,leaked:[]})
  }
})


test('the existing upstream patch authority retains exact skipped operation identities and anonymous groups',()=>{
  const first='- insert:\n  - {id: group, group: true, config: []}\n  - {id: docs-entry, name: portable}\n'
  const second='- {id: missing-group, insert: []}\n- {disabled: true}\n- {id: missing-row, disabled: true}\n- {id: docs-entry, name: wrong-package, disabled: true}\n- id: group\n  insert:\n  - {id: child, name: ./row.mjs}\n'
  const spec={version:1,layers:[{path:'first.yml',source:first,sha256:hash(first)},{path:'overlay.yml',source:second,sha256:hash(second)}],modules:[],files:{}}
  const result=spawnSync(process.execPath,[fileURLToPath(new URL('../dist/dsh-composition-review.mjs',import.meta.url))],{input:JSON.stringify(spec),encoding:'utf8',timeout:5000})
  assert.equal(result.status,0,result.stderr)
  const reviewed=JSON.parse(result.stdout)
  assert.deepEqual(reviewed.skipped.map(row=>[row.row??null,row.layer,row.patch]),[['missing-group','overlay.yml',1],[null,'overlay.yml',2],['missing-row','overlay.yml',3],['docs-entry','overlay.yml',4]])
  assert.equal(reviewed.entries[1].disabled,undefined)
  assert.equal(reviewed.entries[0].name,undefined,'raw patch comparison keeps its original anonymous identity')
  assert.equal(reviewed.entries[0].config[0].name,'./row.mjs')
})

test('roster inventory uses real selected fiber facts and unmounted conditional rows without another graph',async t=>{
  const host=await startHost();t.after(()=>host.stop())
  const source=`export const inject=['tools','agentPresets'];export function apply(ctx){ctx.tools.register({name:'preset_inspect',description:'readonly roster',parameters:{type:'object',properties:{}},execute:async()=>({roster:await ctx.agentPresets.remoteExportList(),inventory:await ctx.agentPresets.compositionInventory(),selected:ctx.agentPresets.composedPreset()})})}`
  const contents={'presets/a/agent.cordis.yml':'- name: ./row.mjs\n','presets/a/row.mjs':source,'presets/b/agent.cordis.yml':"- name: missing-conditional\n  disabled: !!js 'true'\n"}
  const input=request(cfg,contents)
  const top=JSON.stringify([{insert:[{id:'anonymous',group:true,config:JSON.parse(input.composition.layers[0].source)[0].insert}]}])
  input.composition.layers[0]={path:'bundle.json',source:top,sha256:hash(top)}
  const review=await reviewAgentPresets(input);review.top=input.composition
  const [a]=installedEntries(t,review,contents)
  const mounted=await activate(host,'inventory-roster',a.path,{scope:a})
  assert.equal(mounted.result.status,'ok',mounted.result.diagnostic+' '+host.stderr+' '+JSON.stringify(host.logs))
  const tool=host.registry.find(r=>r.op==='register' && r.kind==='tool')
  const result=await host.call('tool/call',{handle:tool.handle,input:{},call_id:'inventory',deadline_ms:5000})
  assert.equal(result.structured.selected,'a')
  assert.equal(result.structured.roster.authorable,false)
  assert.equal(result.structured.roster.defaultId,'a')
  assert.equal(result.structured.inventory[0].isDefault,true)
  assert.equal(result.structured.inventory[0].rows[0].enabled,true)
  assert.ok(Object.hasOwn(result.structured.inventory[0].rows[0],'fiberState'))
  assert.equal(result.structured.inventory[1].rows[0].enabled,'conditional')
  assert.equal(Object.hasOwn(result.structured.inventory[1].rows[0],'fiberState'),false)
  assert.deepEqual(await host.call('ext/deactivate',{owner:mounted.ref}),{disposed:true,leaked:[]})
})


test('real raw persona rows admit stock additive templates under exact sibling scopes', async t => {
  const contents = {
    'presets/a/agent.cordis.yml': '- name: "@deepseek-ai/dsh-persona"\n  config: {prefix: "You are {{model}}.", suffix: "Work in {{cwd}}."}\n',
    'presets/b/agent.cordis.yml': '- name: "@deepseek-ai/dsh-persona"\n  config: {prefix: "B {{model}}", suffix: "{{cwd}}"}\n',
    'presets/replacement/agent.cordis.yml': '- name: "@deepseek-ai/dsh-persona"\n  config: {prefix: replace, complete: true}\n',
    'presets/suppression/agent.cordis.yml': '- name: "@deepseek-ai/dsh-persona"\n  config: {prefix: suppress, includeRuntimeContext: false}\n',
    'presets/unknown/agent.cordis.yml': '- name: "@deepseek-ai/dsh-persona"\n  config: {prefix: "{{env}}"}\n',
  }
  const input = request(cfg, contents), review = await reviewAgentPresets(input); review.top = input.composition
  assert.deepEqual(review.presets.map(row=>row.metadata.id), ['a','b'])
  assert.match(review.catalog.presets.find(row=>row.id==='replacement').broken,/Core prompt/)
  assert.match(review.catalog.presets.find(row=>row.id==='suppression').broken,/Core prompt/)
  assert.match(review.catalog.presets.find(row=>row.id==='unknown').broken,/unknown Core prompt variable/)
  assert.ok(review.presets.every(row=>row.composition.modules.length===0),'fixed bridge requires no ambient persona package')
  const [a,b] = installedEntries(t, review, contents)
  const host = await startHost(); t.after(()=>host.stop())
  const first = await activate(host,'raw-personas',a.path,{scope:a})
  assert.equal(first.result.status,'ok',first.result.diagnostic+' '+host.stderr)
  const second = await host.call('ext/activate',{owner:first.ref,plugin_name:'raw-personas',entry:b,scope:b,config:{}})
  assert.equal(second.status,'ok',second.diagnostic)
  const rows = host.registry.filter(row=>row.op==='register' && row.kind==='prompt_template')
  assert.equal(rows.length,4)
  for (const scope of [a,b]) assert.deepEqual(rows.filter(row=>row.scope.path===scope.path).map(row=>row.spec.name),['persona-prefix','persona-suffix'])
  assert.ok(rows.every(row=>row.spec.description.includes('{{')),'only Core may expand actual turn facts')
  assert.deepEqual(await host.call('ext/deactivate',{owner:first.ref,entry:a}),{disposed:true,leaked:[]})
  for (const row of rows) assert.equal(host.registry.some(event=>event.op==='unregister' && event.handle===row.handle),row.scope.path===a.path)
  assert.deepEqual(await host.call('ext/deactivate',{owner:first.ref}),{disposed:true,leaked:[]})
})
