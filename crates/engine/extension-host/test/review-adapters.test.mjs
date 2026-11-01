import test from 'node:test'
import assert from 'node:assert/strict'
import { dirname, join } from 'node:path'
import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { transformStockSnapshot } from '../dist/stock-adapters.mjs'
import { createHarnessModule } from '../dist/builtin/harness.mjs'
import { startHost, BUNDLE } from './harness.mjs'
const LIMIT=16*1024*1024
const fixture=JSON.parse(readFileSync(new URL('../../tests/fixtures/review-host-parity.json',import.meta.url),'utf8'))
const snapshot=(operation,input)=>({kind:'stock_adapter',operation,input})
const transform=(operation,input)=>transformStockSnapshot(snapshot(operation,input))
const owner={plugin_id:'host:harness',generation:4,owner_token:'fixture-exact-review-job'}
const params=id=>({owner,execution_id:id,ticket:'opaque-core-execution-grant',deadline_ms:10000})
const part={number:1,diff_fingerprint:'sha256:'+'a'.repeat(64),diff_chars:42,file_count:1,files:['a/x b/x']}
const manifest={base_sha:'b'.repeat(40),head_sha:'a'.repeat(40),diff_fingerprint:'sha256:'+'c'.repeat(64),diff_chars:42,file_count:1,binary_file_patches:0,binary_contents_semantically_inspected:false,max_chars_per_pass:40000,passes:[part],skipped_files:[]}
const passInput=()=>({number:7,view:{title:'untrusted title',body:'untrusted body'},manifest,pass:part,diff:'diff --git a/x b/x\n+END\n',context:{files:[{path:'x',source:'漢字e\u0301',head_sha:manifest.head_sha}],limit:64},sort_keys:true})
const sorted=value=>Array.isArray(value)?value.map(sorted):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).sort(([a],[b])=>a<b?-1:a>b?1:0).map(([key,value])=>[key,sorted(value)])):value
const envelopeBytes=value=>8+Buffer.byteLength(`{"jsonrpc":"2.0","id":18446744073709551615,"result":${JSON.stringify(value)}}`)

test('frozen Core formatter counterpart matches all sixteen source interactive and local/posted report cases',()=>{
 for(const {operation,input,content} of fixture.cases) assert.equal(transform(operation,input).result.content,content)
})
test('file numbering uses scalar source lines, CRLF and terminal lone CR without truncation',()=>{
 const content=Array.from({length:10001},(_,i)=>i===10000?'漢字🐋e\u0301END':'x').join('\r\n')+'\r'
 const output=transform('review_source_prompt',{kind:'file',display:'x',content}).result.content
 assert.ok(output.includes('10000 | x\n10001 | 漢字🐋e\u0301END\r\n\nEnd of file.'))
 assert.ok(!output.includes('...'))
})
test('complete pass preserves exact Core manifest, source provenance and serialized map order',()=>{
 const input=passInput(), content=transform('review_pass_prompt',input).result.content,parsed=JSON.parse(content)
 assert.deepEqual(parsed.manifest,input.manifest);assert.deepEqual(parsed.pass,input.pass);assert.deepEqual(parsed.repository_context,input.context)
 assert.deepEqual(parsed.pull_request,{number:7,title:input.view.title,description:input.view.body});assert.equal(parsed.diff,input.diff)
 assert.equal(parsed.untrusted_repository_data,true);assert.ok(parsed.task.includes('No build or tests have been run.'))
 assert.equal(content,JSON.stringify(sorted(parsed)))
 input.sort_keys=false;const preserved=transform('review_pass_prompt',input).result.content
 assert.ok(preserved.startsWith('{"task":'));assert.deepEqual(JSON.parse(preserved),parsed)
})
test('partial pass explicitly carries every unread file and never invents full coverage',()=>{
 const input=passInput();input.manifest={...manifest,skipped_files:[{file:'a/unread b/unread',chars:30000,reason:'beyond the max_passes budget'}]};input.context=null
 const parsed=JSON.parse(transform('review_pass_prompt',input).result.content)
 assert.ok(parsed.task.includes('partial review (pass 1 of 1)'));assert.ok(parsed.task.includes('a/unread b/unread (30000 chars; beyond the max_passes budget)'));assert.ok(parsed.task.includes('Do not claim full coverage.'))
 assert.equal(parsed.repository_context,null);assert.deepEqual(parsed.manifest,input.manifest)
})
test('complete captured review larger than one MiB is accepted with the final patch intact',()=>{
 const diff='漢'.repeat(400000)+'FINAL_PATCH_END',input={kind:'cli_diff',diff}
 assert.ok(Buffer.byteLength(JSON.stringify(snapshot('review_source_prompt',input)))>1024*1024)
 const result=transform('review_source_prompt',input).result
 assert.ok(result.content.endsWith('FINAL_PATCH_END\n\nEnd of diff.'));assert.equal(result.success,true);assert.equal(result.metadata,null)
})
test('serialized review snapshot over its purpose cap refuses even when raw source fits',()=>{
 const input={kind:'cli_diff',diff:'\0'.repeat(3*1024*1024)}
 assert.ok(Buffer.byteLength(input.diff)<LIMIT);assert.ok(envelopeBytes(snapshot('review_source_prompt',input))>LIMIT)
 assert.throws(()=>transform('review_source_prompt',input),/review.*16 MiB/)
})
test('nested prompt result escaping is separately bounded without dropping context',()=>{
 const input=passInput();input.context={source:'\0'.repeat(2500000)}
 assert.ok(envelopeBytes(snapshot('review_pass_prompt',input))<LIMIT)
 assert.throws(()=>transform('review_pass_prompt',input),/review result exceeds 16 MiB/)
})
test('other helpers retain one MiB and unknown review operations never obtain a larger allowance',()=>{
 for(const operation of ['speech_options','review_publish']) assert.throws(()=>transform(operation,{payload:'x'.repeat(1024*1024+1)}),/1 MiB/)
 for(const [operation,input] of [['review_source_prompt',{kind:'url',url:'https://example.invalid'}],['review_pass_prompt',{...passInput(),sort_keys:null}],['review_pass_prompt',{...passInput(),context:undefined}],['review_report',{review:[],output:'',posted:false}],['review_interactive_pr',{number:2**53,view:{},diff:''}]]) assert.throws(()=>transform(operation,input))
})
test('raw prose is returned byte-exact and presenter cannot grant metadata or approval',()=>{
 const input={review:null,output:'\0raw ```suggestion\nunknown\n```\n',posted:true},result=transform('review_report',input).result
 assert.equal(result.content,input.output);assert.equal(result.metadata,null);assert.equal(result.success,true)
})
test('actual module redeems one opaque grant for every closed review operation',async()=>{
 const cases=[fixture.cases[0],{operation:'review_pass_prompt',input:passInput()},fixture.cases.find(x=>x.operation==='review_interactive_pr'),fixture.cases.find(x=>x.operation==='review_report')]
 for(const [i,entry] of cases.entries()) {
  const calls=[],runner=createHarnessModule({async request(method,args){calls.push([method,args]);return snapshot(entry.operation,entry.input)}},owner)
  assert.deepEqual(await runner.run(params(`case${i}`),new AbortController().signal),transform(entry.operation,entry.input))
  assert.deepEqual(calls,[['exec/redeem',{owner,execution_id:`case${i}`,ticket:'opaque-core-execution-grant'}]]);await runner.dispose()
 }
})
test('cancellation and disposed owner suppress late review completion',async()=>{
 let finish;const runner=createHarnessModule({request(){return new Promise(resolve=>{finish=resolve})}},owner),controller=new AbortController()
 const pending=runner.run(params('cancel'),controller.signal);controller.abort();await assert.rejects(pending,/cancelled/)
 finish(snapshot('review_source_prompt',{kind:'cli_diff',diff:'late'}));await runner.dispose()
 await assert.rejects(runner.run(params('disposed'),new AbortController().signal),/stale/)
})
test('real builtin Host process presents all four adopted review operations over existing wire',async t=>{
 const host=await startHost({tier:'builtin'});t.after(()=>host.stop())
 const entry=join(dirname(BUNDLE),'builtin','harness.mjs'),sha256=createHash('sha256').update(readFileSync(entry)).digest('hex')
 assert.equal((await host.call('ext/activate',{owner,plugin_name:'Pinned review presenter',entry:{path:entry,sha256},config:{}})).status,'ok')
 const cases=[fixture.cases[0],{operation:'review_pass_prompt',input:passInput()},fixture.cases.find(x=>x.operation==='review_interactive_pr'),fixture.cases.find(x=>x.operation==='review_report'),{operation:'review_source_prompt',input:{kind:'cli_diff',diff:'漢'.repeat(400000)+'WIRE_END'}}]
 for(const [i,row] of cases.entries()) {
  const redeemed=host.waitFor(message=>message.method==='exec/redeem'),result=host.call('harness/run',params(`wire${i}`)),request=await redeemed
  assert.deepEqual(request.params,{owner,execution_id:`wire${i}`,ticket:'opaque-core-execution-grant'})
  host.send({jsonrpc:'2.0',id:request.id,result:snapshot(row.operation,row.input)})
  assert.deepEqual(await result,transform(row.operation,row.input))
 }
 assert.deepEqual(await host.call('ext/deactivate',{owner}),{disposed:true,leaked:[]})
})
test('real Host refuses encoded purpose overflow then remains usable without retrying the grant',async t=>{
 const host=await startHost({tier:'builtin'});t.after(()=>host.stop())
 const entry=join(dirname(BUNDLE),'builtin','harness.mjs'),sha256=createHash('sha256').update(readFileSync(entry)).digest('hex')
 assert.equal((await host.call('ext/activate',{owner,plugin_name:'Bounded review presenter',entry:{path:entry,sha256},config:{}})).status,'ok')
 for(const [i,diff] of ['\0'.repeat(3*1024*1024),'accepted_after_refusal'].entries()) {
  const redeemed=host.waitFor(message=>message.method==='exec/redeem'),result=host.call('harness/run',params(`bounded${i}`)),request=await redeemed
  host.send({jsonrpc:'2.0',id:request.id,result:snapshot('review_source_prompt',{kind:'cli_diff',diff})})
  if(i===0) await assert.rejects(result,/review.*16 MiB/)
  else assert.ok((await result).result.content.includes(diff))
 }
 assert.equal(host.coreCalls.length,0);assert.deepEqual(await host.call('ext/deactivate',{owner}),{disposed:true,leaked:[]})
})
