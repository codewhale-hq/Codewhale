// Acceptance candidates only. Real host/fake core, not Rust admission or grants proof.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { activate, startHost } from './harness.mjs'

function entries(t) {
  const root=mkdtempSync(join(tmpdir(),'cw-entry-scopes-'))
  t.after(()=>rmSync(root,{recursive:true,force:true}))
  const make=(name,body)=>{
    const path=join(root,`${name}.mjs`)
    const source=`export const inject=['tools','prompt'];export function apply(ctx) { ${body} }`
    writeFileSync(path,source)
    return {path,sha256:createHash('sha256').update(source).digest('hex')}
  }
  return {root,make}
}

test('one owner can mount two scoped entries with equal names and dispose only one',async t=>{
  const host=await startHost();t.after(()=>host.stop())
  const {root,make}=entries(t)
  const body=value=>`ctx.tools.register({name:'scoped_echo',description:'Entry echo',parameters:{type:'object',properties:{}},execute(){return ${JSON.stringify(value)}}});ctx.prompt.registerSection({id:'note',text:${JSON.stringify(value)}})`
  const a=make('a',body('A')),b=make('b',body('B'))
  const first=await activate(host,'scoped-entries',a.path,{scope:a,data_dir:root})
  assert.equal(first.result.status,'ok',first.result.diagnostic)
  const second=await host.call('ext/activate',{owner:first.ref,plugin_name:'scoped-entries',entry:b,scope:b,config:{},data_dir:root})
  assert.equal(second.status,'ok',second.diagnostic)
  const tools=host.registry.filter(row=>row.op==='register' && row.kind==='tool')
  assert.equal(tools.length,2)
  assert.deepEqual(tools.map(row=>row.scope),[a,b])
  assert.notEqual(tools[0].handle,tools[1].handle)
  const call=handle=>host.call('tool/call',{handle,input:{},call_id:`scope-${handle}`,deadline_ms:5000})
  assert.match(JSON.stringify(await call(tools[0].handle)),/A/)
  assert.match(JSON.stringify(await call(tools[1].handle)),/B/)
  assert.deepEqual(await host.call('ext/deactivate',{owner:first.ref,entry:a}),{disposed:true,leaked:[]})
  await assert.rejects(call(tools[0].handle),/not live/)
  assert.match(JSON.stringify(await call(tools[1].handle)),/B/)
  assert.ok(host.registry.some(row=>row.op==='unregister' && row.handle===tools[0].handle))
  assert.ok(!host.registry.some(row=>row.op==='unregister' && row.handle===tools[1].handle))
  assert.deepEqual(await host.call('ext/deactivate',{owner:first.ref}),{disposed:true,leaked:[]})
  await assert.rejects(call(tools[1].handle),/not live/)
})

test('failed scoped activation keeps the same owner sibling live and mismatched scopes refuse',async t=>{
  const host=await startHost();t.after(()=>host.stop())
  const {root,make}=entries(t)
  const good=make('good',"ctx.tools.register({name:'kept_echo',description:'kept',parameters:{type:'object',properties:{}},execute(){return 'kept'}})")
  const bad=make('bad',"ctx.prompt.registerSection({id:'partial',text:'must retire'});throw new Error('intentional scoped failure')")
  const first=await activate(host,'scoped-failure',good.path,{scope:good,data_dir:root})
  assert.equal(first.result.status,'ok',first.result.diagnostic)
  const refused=await host.call('ext/activate',{owner:first.ref,plugin_name:'scoped-failure',entry:bad,scope:good,config:{},data_dir:root})
  assert.equal(refused.status,'failed');assert.match(refused.diagnostic,/scope does not match/)
  const failed=await host.call('ext/activate',{owner:first.ref,plugin_name:'scoped-failure',entry:bad,scope:bad,config:{},data_dir:root})
  assert.equal(failed.status,'failed');assert.match(failed.diagnostic,/intentional scoped failure/)
  assert.deepEqual(await host.call('ext/deactivate',{owner:first.ref,entry:bad}),{disposed:true,leaked:[]})
  const partial=host.registry.find(row=>row.op==='register' && row.kind==='prompt_section')
  assert.ok(host.registry.some(row=>row.op==='unregister' && row.handle===partial.handle))
  const tool=host.registry.find(row=>row.op==='register' && row.kind==='tool')
  assert.match(JSON.stringify(await host.call('tool/call',{handle:tool.handle,input:{},call_id:'kept',deadline_ms:5000})),/kept/)
  assert.deepEqual(await host.call('ext/deactivate',{owner:first.ref}),{disposed:true,leaked:[]})
})
