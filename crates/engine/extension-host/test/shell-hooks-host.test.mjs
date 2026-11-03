// Real selected Node host/HostRoot/Loader; fake Core register authority. No shell process runs.
import test from 'node:test'
import assert from 'node:assert/strict'
import {realpathSync,mkdtempSync,writeFileSync,mkdirSync,rmSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {createHash} from 'node:crypto'
import {startHost,activate} from './harness.mjs'
const hash=value=>createHash('sha256').update(value).digest('hex')
function composition(t,dialect,{bad=false}={}) {
 const root=realpathSync(mkdtempSync(join(tmpdir(),'cw-hook-host-')))
 t.after(()=>rmSync(root,{recursive:true,force:true}));mkdirSync(join(root,'source'))
 const config=JSON.stringify({hooks:{PreToolUse:[{matcher:'write',hooks:[{type:'command',command:'true',timeout:3}]}]}})
 writeFileSync(join(root,'source','hooks.json'),config)
 const layer=JSON.stringify([{insert:[{id:'dialect-hooks',name:`@deepseek-ai/dsh-hooks-${dialect}`,config:{configPath:'hooks.json'}}]}])
 const spec={version:1,layers:[{path:'base.json',sha256:hash(layer),source:layer}],modules:[],files:{'hooks.json':hash(config)}}
 writeFileSync(join(root,'composition.json'),JSON.stringify(spec))
 const entry=join(root,'index.mjs')
 writeFileSync(entry,`import {mountReviewedComposition} from '@codewhale/dsh-composition';import spec from './composition.json' with {type:'json'};export async function apply(ctx){await mountReviewedComposition(ctx,new URL('./source/',import.meta.url).href,spec)}`)
 if(bad)writeFileSync(join(root,'source','hooks.json'),'{}')
 return entry
}

test('reviewed Claude and Codex bridge rows mount in one real HostRoot and withdraw exact owner handles',async t=>{
 const host=await startHost();t.after(()=>host.stop())
 const a=await activate(host,'claude-native-hooks',composition(t,'claude-code'))
 const b=await activate(host,'codex-native-hooks',composition(t,'codex'))
 assert.equal(a.result.status,'ok',a.result.diagnostic+' '+JSON.stringify(host.logs)+' '+host.stderr);assert.equal(b.result.status,'ok',b.result.diagnostic)
 const rows=host.registry.filter(r=>r.op==='register' && r.kind==='shell_hook')
 assert.equal(rows.length,2);assert.deepEqual(rows.map(r=>r.owner.plugin_id),['claude-native-hooks','codex-native-hooks'])
 for(const row of rows){const spec=JSON.parse(row.spec.description);assert.equal(spec.point,'PreToolUse');assert.equal(spec.matcher,'write');assert.equal(spec.hook.command,'true');assert.equal(spec.hook.event,'tool_call_before')}
 assert.deepEqual(await host.call('ext/deactivate',{owner:a.ref}),{disposed:true,leaked:[]})
 assert.ok(host.registry.some(r=>r.op==='unregister' && r.handle===rows[0].handle))
 assert.ok(!host.registry.some(r=>r.op==='unregister' && r.handle===rows[1].handle))
 assert.deepEqual(await host.call('ext/deactivate',{owner:b.ref}),{disposed:true,leaked:[]})
 assert.ok(host.registry.some(r=>r.op==='unregister' && r.handle===rows[1].handle))
})

test('changed reviewed config fails activation without empty-tree success or harming another hook owner',async t=>{
 const host=await startHost();t.after(()=>host.stop())
 const good=await activate(host,'good-hooks',composition(t,'claude-code'))
 assert.equal(good.result.status,'ok',good.result.diagnostic+' '+JSON.stringify(host.logs)+' '+host.stderr)
 const goodRow=host.registry.find(r=>r.op==='register' && r.kind==='shell_hook')
 const changed=await activate(host,'changed-hooks',composition(t,'codex',{bad:true}))
 assert.equal(changed.result.status,'failed');assert.match(changed.result.diagnostic,/changed|closure|refused/i)
 assert.equal(host.registry.filter(r=>r.op==='register' && r.owner.plugin_id==='changed-hooks').length,0)
 assert.ok(!host.registry.some(r=>r.op==='unregister' && r.handle===goodRow.handle))
 assert.deepEqual(await host.call('ext/deactivate',{owner:good.ref}),{disposed:true,leaked:[]})
 assert.ok(host.registry.some(r=>r.op==='unregister' && r.handle===goodRow.handle))
})
