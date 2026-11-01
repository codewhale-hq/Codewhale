// Real Node host/wire; fake Core authority. Production cleanup regression receipts.
import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { startHost, owner, sha256File } from './harness.mjs'
function entry(dir, name, source) { const path = join(dir, name + '.mjs'); writeFileSync(path, source); return { path, sha256: sha256File(path) } }
async function setup(t) { const dir = mkdtempSync(join(tmpdir(), 'cw-activation-cleanup-')); const host = await startHost(); t.after(async () => { await host.stop(); rmSync(dir, {recursive:true,force:true}) }); return {dir,host,ref:owner('native-cleanup')} }
const scoped = (ref, path) => ({owner:ref,plugin_name:'native-cleanup',entry:path,scope:path,config:{}})

test('scoped failure preserves original refusal and a working sibling', async t => {
  const {dir,host,ref}=await setup(t)
  const good=entry(dir,'good',`export const inject=['tools']; export function apply(ctx){ctx.tools.register({name:'sibling',description:'live',parameters:{type:'object'},execute:()=> 'sibling live'})}`)
  const bad=entry(dir,'bad',`export function apply(ctx){ctx.provide('approval',{})}`)
  assert.equal((await host.call('ext/activate',scoped(ref,good))).status,'ok')
  const result=await host.call('ext/activate',scoped(ref,bad)); assert.equal(result.status,'failed'); assert.match(result.diagnostic,/approval/)
  assert.deepEqual(await host.call('ext/deactivate',{owner:ref,entry:bad}),{disposed:true,leaked:[]})
  const registration=host.registry.find(row=>row.op==='register' && row.spec.name==='sibling')
  const answer=await host.call('tool/call',{handle:registration.handle,call_id:'sibling-call',input:{},deadline_ms:1000})
  assert.equal(answer.content[0].text,'sibling live')
})
test('failed scoped activation retains incomplete disposer for teardown receipt', {timeout:15000}, async t => {
  const {dir,host,ref}=await setup(t)
  const bad=entry(dir,'dirty',`export function apply(ctx){ctx.effect(()=>()=>new Promise(()=>{}),'unfinished failed activation');ctx.provide('approval',{})}`)
  const result=await host.call('ext/activate',scoped(ref,bad)); assert.equal(result.status,'failed')
  const receipt=await host.call('ext/deactivate',{owner:ref,entry:bad}); assert.equal(receipt.disposed,false,'a missing owner must not manufacture clean teardown')
  assert.deepEqual(await host.call('ext/deactivate',{owner:ref}),{disposed:true,leaked:[]})
})
test('withdrawal during async activation cannot return late success', async t => {
  const {dir,host,ref}=await setup(t)
  const late=entry(dir,'late',`export async function apply(ctx){ctx.logger.info('late activation reached await');await new Promise(resolve=>setTimeout(resolve,100));}`)
  const begun=host.request('ext/activate',scoped(ref,late))
  await host.waitFor(message=>message.method==='log' && JSON.stringify(message.params).includes('late activation reached await'))
  await host.call('ext/deactivate',{owner:ref,entry:late})
  const answer=await begun.promise; assert.equal(answer.status,'failed'); assert.match(answer.diagnostic,/withdrawn/)
})
