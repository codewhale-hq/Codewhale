// Real HostRoot/Loader; Core admission is a recorded fake. Rust pool tests are separate.
import {test} from 'node:test'
import assert from 'node:assert/strict'
import {join} from 'node:path'
import {mkdtempSync,writeFileSync,rmSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {startHost,activate,owner,sha256File,FIXTURES} from './harness.mjs'
const directory=join(FIXTURES,'raw-dsh-mcp')
const entry=tag=>{const path=join(directory,'native',`${tag}.mjs`);return {path,sha256:sha256File(path)}}
function plugin(t,body){const dir=mkdtempSync(join(tmpdir(),'cw-mcp-proposal-'));t.after(()=>rmSync(dir,{recursive:true,force:true}));const path=join(dir,'index.mjs');writeFileSync(path,`export const inject=['mcp'];export function apply(ctx){${body}}`);return path}

test('raw MCP and skills mount with all authored registries under exact sibling scopes',async t=>{
 const host=await startHost();t.after(()=>host.stop());const ref=owner('raw-dsh-mcp')
 for(const tag of ['a','b']) {const e=entry(tag);const result=await activate(host,'raw-dsh-mcp',e.path,{owner:ref,scope:e});assert.equal(result.result.status,'ok',result.result.diagnostic)}
 const registered=host.registry.filter(r=>r.op==='register')
 assert.deepEqual([...new Set(registered.map(r=>r.kind))].sort(),['command','hook','mcp_server','prompt_section','skill_root','tool'])
 for(const tag of ['a','b']) {
  const scoped=registered.filter(r=>r.scope.path===entry(tag).path);assert.equal(scoped.length,6)
  const mcp=scoped.find(r=>r.kind==='mcp_server');assert.equal(mcp.spec.name,'scoped')
  assert.deepEqual(JSON.parse(mcp.spec.description),{type:'stdio',command:'node',args:['peer.mjs',tag],env:{},cwd:'source',extensions:{'net.codewhale':{execute_timeout:5}}})
  assert.equal(scoped.find(r=>r.kind==='skill_root').spec.name,'source/skills')
 }
 await host.call('ext/deactivate',{owner:ref,entry:entry('a')})
 const disposed=new Set(host.registry.filter(r=>r.op==='unregister').map(r=>r.handle))
 for(const item of registered) assert.equal(disposed.has(item.handle),item.scope.path===entry('a').path)
 const remaining=registered.find(r=>r.scope.path===entry('b').path&&r.kind==='tool')
 const result=await host.call('tool/call',{handle:remaining.handle,input:{},call_id:'sibling',deadline_ms:5000});assert.equal(result.content[0].text,'b')
})

test('Core MCP refusal rolls back every mixed graph proposal before failed activation',async t=>{
 const host=await startHost({admit:spec=>spec.name==='scoped'?{refused:'fixture reviewed definition refused'}:undefined});t.after(()=>host.stop())
 const e=entry('a');const {ref,result}=await activate(host,'raw-dsh-mcp',e.path,{scope:e})
 assert.equal(result.status,'failed');assert.match(result.diagnostic,/fixture reviewed definition refused/)
 await host.call('ext/deactivate',{owner:ref,entry:e})
 for(const item of host.registry.filter(r=>r.op==='register'&&r.kind!=='mcp_server'))assert.ok(host.registry.some(r=>r.op==='unregister'&&r.handle===item.handle))
})

test('literal credentials and duplicate namespaces fail before transport or credential protocol',async t=>{
 const host=await startHost();t.after(()=>host.stop())
 const path=plugin(t,`ctx.mcp.registerServer({serverName:'bad',server:{type:'streamable-http',url:'https://example.invalid/mcp',headers:{Authorization:'fixture-canary'}}})`)
 const {result}=await activate(host,'credential-refused',path);assert.equal(result.status,'failed');assert.match(result.diagnostic,/Rust authentication authority/)
 assert.equal(host.registry.length,0);assert.equal(host.coreCalls.length,0);assert.ok(!host.logs.some(r=>r.msg.includes('fixture-canary')))
 const duplicate=plugin(t,`const definition={serverName:'echo',server:{type:'streamable-http',url:'https://example.invalid/mcp'}};ctx.mcp.registerServer(definition);ctx.mcp.registerServer(definition)`)
 const failed=await activate(host,'duplicate-mcp',duplicate);assert.equal(failed.result.status,'failed');assert.match(failed.result.diagnostic,/already registered/)
 await host.call('ext/deactivate',{owner:failed.ref});assert.ok(host.registry.some(r=>r.op==='unregister'))
})

test('a raw Native module cannot install a competing runtimeLoop service',async t=>{
 const host=await startHost();t.after(()=>host.stop())
 const path=plugin(t,`ctx.provide('runtimeLoop',{turn(){}})`)
 const {result}=await activate(host,'runtime-refused',path)
 assert.equal(result.status,'failed');assert.match(result.diagnostic,/runtimeLoop.*core owns/)
 assert.equal(host.coreCalls.length,0);assert.equal(host.registry.length,0)
})
