import test from 'node:test'
import assert from 'node:assert/strict'
import { dirname, join } from 'node:path'
import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { transformStockSnapshot } from '../dist/stock-adapters.mjs'
import { createHarnessModule } from '../dist/builtin/harness.mjs'
import { startHost, BUNDLE } from './harness.mjs'
const transform=(operation,input)=>transformStockSnapshot({kind:'stock_adapter',operation,input}).result.metadata
const decode=(backend,parsed,number_facts={},max_results=10)=>transform('web_provider',{backend,parsed,number_facts,max_results})
const a={title:' A ',url:' web-url-0 ',content:'\u0085meaningful\u0085'}, b={title:' B ',url:'web-url-1',snippet:' fallback '}
for(const [backend,parsed,expected] of [
 ['tavily',{results:[a,b]},{title:'A',url:'web-url-0',snippet:'meaningful'}],
 ['firecrawl',{data:{web:[{...a,description:' summary '}]}},{title:'A',url:'web-url-0',snippet:'summary'}],
 ['metaso',{webpages:[{...a,link:a.url,snippet:' snippet '}],code:0},{title:'A',url:'web-url-0',snippet:'snippet'}],
 ['bocha',{data:{webPages:{value:[{name:' A ',url:a.url,summary:' summary '}]}}},{title:'A',url:'web-url-0',snippet:'summary'}],
 ['baidu',{references:[{name:' A ',link:a.url,content:' content '}]},{title:'A',url:'web-url-0',snippet:'content'}],
 ['searxng',{results:[a,b]},{title:'A',url:'web-url-0',snippet:'meaningful'}],
 ['sofya',{results:[a]},{title:' A ',url:' web-url-0 ',snippet:'meaningful'}],
 ['serply',{results:[{...a,link:a.url,description:' description '}]},{title:' A ',url:' web-url-0 ',snippet:'description'}],
 ['volcengine',{output:[{type:'message',content:[{text:'```json\n'+JSON.stringify({results:[{...a,snippet:' source '}]})+'\n```'}]}]},{title:'A',url:'web-url-0',snippet:'source'}],
])test(`${backend} exact aliases, whitespace and candidate order`,()=>assert.deepEqual(decode(backend,parsed).entries[0],expected))
test('present null/wrong fields suppress fallback only for the correct providers',()=>{
 assert.deepEqual(decode('bocha',{pages:[{name:null,title:'fallback',url:'u0'},{name:'B',url:'u2',summary:null,snippet:'ignored'}]}).entries,[{title:'B',url:'u2'}])
 assert.deepEqual(decode('baidu',{references:[{title:'A',url:'u0',content:4,snippet:'ignored'}]}).entries,[{title:'A',url:'u0'}])
 assert.deepEqual(decode('tavily',{results:[{title:'A',url:'u0',content:4,snippet:'used'}]}).entries,[{title:'A',url:'u0',snippet:'used'}])
 assert.deepEqual(decode('firecrawl',{data:{web:null},results:[a]}).entries,[])
})
test('Core integer facts distinguish float/string from exact signed integer API codes',()=>{
 for(const i64 of [null,undefined])assert.equal(decode('metaso',{code:2005},{'/code':{i64}}).error,null)
 assert.match(decode('metaso',{code:2005},{'/code':{i64:'2005'}}).error,/API key rejected/)
 assert.match(decode('metaso',{code:3003},{'/code':{i64:'3003'}}).error,/daily search limit/)
 assert.equal(decode('bocha',{code:200},{'/code':{i64:'200'}}).error,null)
 assert.equal(decode('baidu',{error_code:null,code:9},{'/error_code':{i64:null},'/code':{i64:'9'}}).error,null)
})
test('stable score sort includes positive versus negative zero',()=>{
 const parsed={results:['negative','zero','high','equal'].map((title,i)=>({title,url:`u${i}`}))}
 const facts=Object.fromEntries(['-0','0','1e+300','1e+300'].map((score,i)=>[`/results/${i}/score`,{score}]))
 assert.deepEqual(decode('searxng',parsed,facts,3).entries.map(value=>value.title),['high','equal','zero'])
})
test('Firecrawl scalar cap keeps astral glyphs and Rust trim keeps BOM',()=>{
 assert.equal([...decode('firecrawl',{data:[{title:'\ufeffA',url:'u',description:'😀'.repeat(1001)}]}).entries[0].snippet].length,1000)
 assert.equal(decode('tavily',{results:[{title:'\ufeff',url:'u'}]}).entries[0].title,'\ufeff')
})
test('Volcengine missing text, null error, latest message and invalid inner JSON are distinct',()=>{
 assert.match(decode('volcengine',{error:null}).error,/code unknown: no details/)
 assert.match(decode('volcengine',{output:[{type:'reasoning',content:[{text:'{}'}]}]}).error,/no output text/)
 assert.deepEqual(decode('volcengine',{output:[{type:'message',content:[{text:'not JSON'}]}]}).entries,[])
 assert.deepEqual(decode('volcengine',{output:[{type:'message',content:[{text:'{"results":[{"title":"old","url":"u"}]}'}]},{type:'message',content:[{text:'{"results":[]}'}]}]}).entries,[])
})
test('all eleven providers use keyless plans without endpoint, model or credential authority',()=>{
 for(const backend of ['firecrawl','tavily','bocha','metaso','sofya','baidu','volcengine','serply','searxng','bing','duckduckgo']){
  const plan=transform('web_request',{backend,query:'authorized query',max_results:5,filters:transform('web_filters',{recency_days:10,locale:' de_DE '}),locale:'de_DE'})
  assert.equal(plan.kind,'web_request');assert.equal(JSON.stringify(plan).includes('api_key'),false);assert.equal(JSON.stringify(plan).includes('Authorization'),false)
 }
 assert.deepEqual(transform('web_request',{backend:'serply',query:'q',max_results:5,filters:transform('web_filters',{locale:'zh-CN',recency_days:null})}),{kind:'web_request',payload:null,pairs:[['q','q'],['num','5'],['hl','zh'],['gl','cn']]})
})
test('filter rounding uses exact Rust whitespace and language/region rules',()=>{
 for(const [recency_days,window]of [[1,'day'],[2,'week'],[7,'week'],[8,'month'],[31,'month'],[32,'year'],[3650,'year']])assert.equal(transform('web_filters',{recency_days,locale:null}).window,window)
 assert.deepEqual(transform('web_filters',{recency_days:null,locale:'\u0085ZH_cn\u0085'}),{kind:'web_filters',window:null,language:'zh',region:'CN'})
 assert.equal(transform('web_filters',{locale:'\ufeffen-US'}).language,null)
})
test('receipt composes ignored knobs around mandatory Core domain facts and opaque note presence',()=>{
 const got=transform('web_finalize',{requested:{recency:true,domains:true,locale:true},capabilities:{max_results:'supported',recency:'unsupported',locale:'supported'},count:2,degraded:[{kind:'answer_cut_by_provider'}],domain_extra:[{kind:'post_filtered',knob:'domains'}],has_note:true})
 assert.deepEqual(got.honored,{max_results:true,recency:false,domains:true,locale:true});assert.equal(got.prefix,'Found 2 result(s). ');assert.match(got.suffix,/incomplete/)
 assert.deepEqual(got.degraded,[{kind:'answer_cut_by_provider'},{kind:'knob_ignored',knob:'recency'},{kind:'post_filtered',knob:'domains'}])
})
test('malformed and oversized proposals refuse rather than return partial/empty results',()=>{
 for(const [op,value]of [['web_provider',{backend:'unknown',parsed:{},max_results:5,number_facts:{}}],['web_images',{max_results:5,parsed:{results:[{image:'u',width:-1}]}}],['web_filters',{recency_days:3651}],['web_entries',{entries:[{title:4,url:'u'}]}]])assert.throws(()=>transform(op,value))
 assert.throws(()=>decode('tavily',{results:[{title:'x'.repeat(1024*1024),url:'u'}]}),/1 MiB/)
})
test('image candidates skip blank images before Core source-domain/count filtering',()=>{
 assert.deepEqual(transform('web_images',{max_results:1,parsed:{results:[{image:'\u0085'},{image:'opaque-image',width:4294967295,title:'A',url:'opaque-page'},{image:'opaque-second'}]}}),{kind:'web_images',max_results:1,entries:[{image:'opaque-image',title:'A',url:'opaque-page',width:4294967295},{image:'opaque-second'}]})
})
test('runner cancellation/revocation refuses late web completion with one redemption',async()=>{
 const owner={plugin_id:'host:harness',generation:4,owner_token:'reviewed-builtin'};let calls=0,finish
 const runner=createHarnessModule({request(){calls++;return new Promise(resolve=>finish=resolve)}},owner);const stop=new AbortController()
 const pending=runner.run({owner,execution_id:'web',ticket:'opaque-operation',deadline_ms:1000},stop.signal)
 stop.abort();await assert.rejects(pending,/cancelled/);finish({kind:'stock_adapter',operation:'web_filters',input:{recency_days:null,locale:null}});assert.equal(calls,1);await runner.dispose()
 await assert.rejects(runner.run({owner,execution_id:'again',ticket:'opaque-operation',deadline_ms:1000},new AbortController().signal),/stale/)
})
test('real Builtin Host wire runs the actual web adapter with fake Core and one opaque grant',async t=>{
 const host=await startHost({tier:'builtin'});t.after(()=>host.stop());const owner={plugin_id:'host:harness',generation:4,owner_token:'fixture-web'}
 const entry=join(dirname(BUNDLE),'builtin','harness.mjs'),sha256=createHash('sha256').update(readFileSync(entry)).digest('hex')
 assert.equal((await host.call('ext/activate',{owner,plugin_name:'Pinned web adapter',entry:{path:entry,sha256},config:{}})).status,'ok')
 const redeemed=host.waitFor(message=>message.method==='exec/redeem');const pending=host.call('harness/run',{owner,execution_id:'wire-web',ticket:'opaque-operation',deadline_ms:1000});const message=await redeemed
 assert.deepEqual(message.params,{owner,execution_id:'wire-web',ticket:'opaque-operation'})
 host.send({jsonrpc:'2.0',id:message.id,result:{kind:'stock_adapter',operation:'web_provider',input:{backend:'tavily',max_results:1,parsed:{results:[a]},number_facts:{}}}})
 assert.deepEqual((await pending).result.metadata,decode('tavily',{results:[a]},{},1));assert.deepEqual(await host.call('ext/deactivate',{owner}),{disposed:true,leaked:[]})
})
