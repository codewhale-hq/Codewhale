import test from 'node:test'
import assert from 'node:assert/strict'
import { transformStockSnapshot } from '../dist/stock-adapters.mjs'
import { createHarnessModule } from '../dist/builtin/harness.mjs'
const native = extra => ({ kind:'ocr_process', state:'native', status:'success', can_fallback:false, ...extra })
const tess = extra => ({ kind:'ocr_process', state:'tesseract', status:'complete', success:true, exit_code:0, ...extra })
const metadata = value => transformStockSnapshot(value).result.metadata
const owner = { plugin_id:'host:harness', generation:3, owner_token:'fixture-owner' }
const params = { owner, execution_id:'opaque-ocr-job', ticket:'native-grant', deadline_ms:1000 }

test('Native success, error and missing backend are finite private-text directives', () => {
  assert.deepEqual(metadata(native({})),{kind:'ocr_decision',code:'native_success',trim_end:false})
  assert.deepEqual(metadata(native({status:'error'})),{kind:'ocr_decision',code:'native_error',trim_end:false})
  assert.equal(metadata(native({status:'unavailable'})).code,'no_backend')
  assert.match(metadata(native({status:'unavailable'})).message,/no local OCR backend/)
})
test('OCR fallback is admitted only with a separate exact continuation grant', () => {
  for (const status of ['error','unavailable']) assert.deepEqual(metadata(native({status,can_fallback:true,next_ticket:'tesseract-grant'})),{kind:'ocr_decision',code:'fallback',trim_end:false})
  for (const value of [native({status:'error',can_fallback:true}),native({status:'error',next_ticket:'not-admitted'}),native({can_fallback:true,next_ticket:'extra'}),native({status:'error',can_fallback:true,next_ticket:'x'.repeat(257)})]) assert.throws(()=>metadata(value))
})
test('Tesseract success requests private Rust trimming and failure prefix without private diagnostics', () => {
  assert.deepEqual(metadata(tess({})),{kind:'ocr_decision',code:'tesseract_success',trim_end:true})
  assert.equal(metadata(tess({success:false,exit_code:2})).message,'tesseract failed (exit Some(2)): ')
  assert.equal(metadata(tess({success:false,exit_code:null})).message,'tesseract failed (exit None): ')
  assert.deepEqual(metadata({kind:'ocr_process',state:'tesseract',status:'fault'}),{kind:'ocr_decision',code:'fault',trim_end:false})
})
test('unknown, contradictory, private-byte or oversized OCR projections refuse', () => {
  for (const value of [native({status:'retry'}),native({can_fallback:'yes'}),native({path:'/private/image.png'}),native({stdout:'private text'}),tess({status:'pending'}),tess({exit_code:1.5}),tess({exit_code:2**31}),tess({success:null}),tess({stderr:'x'.repeat(1024*1024)}),tess({next_ticket:'third-launch'}),{kind:'ocr_process',state:'tesseract',status:'fault',stdout:'private'}]) assert.throws(()=>metadata(value))
})
test('actual compiled runner redeems ordered Native and Tesseract grants once', async () => {
  const calls=[]
  const runner=createHarnessModule({async request(method,input) {calls.push([method,input]);return calls.length===1?native({status:'error',can_fallback:true,next_ticket:'tesseract-grant'}):tess({})}},owner)
  const result=await runner.run(params,new AbortController().signal)
  assert.deepEqual(result.result.metadata,{kind:'ocr_decision',code:'tesseract_success',trim_end:true})
  assert.deepEqual(calls,[['exec/redeem',{owner,execution_id:'opaque-ocr-job',ticket:'native-grant'}],['exec/redeem',{owner,execution_id:'opaque-ocr-job',ticket:'tesseract-grant'}]])
  assert.equal(JSON.stringify(calls).includes('path'),false)
  await runner.dispose()
})
test('Native completion or no fallback performs no Tesseract redemption', async () => {
  for (const value of [native({}),native({status:'error'}),native({status:'unavailable'})]) {
    let calls=0
    const runner=createHarnessModule({async request(){calls++;return value}},owner)
    await runner.run(params,new AbortController().signal)
    assert.equal(calls,1);await runner.dispose()
  }
})
test('wrong initial/continuation stage and wrong invocation owner refuse', async () => {
  const first=createHarnessModule({async request(){return tess({})}},owner)
  await assert.rejects(first.run(params,new AbortController().signal),/must begin/);await first.dispose()
  let calls=0
  const second=createHarnessModule({async request(){calls++;return native({status:'error',can_fallback:true,next_ticket:'next-grant'})}},owner)
  await assert.rejects(second.run(params,new AbortController().signal),/wrong stage/)
  assert.equal(calls,2);await second.dispose()
  const forbidden=createHarnessModule({async request(){throw new Error('must never reach RPC')}},owner)
  await assert.rejects(forbidden.run({...params,owner:{...owner,owner_token:'wrong-owner'}},new AbortController().signal),/stale/);await forbidden.dispose()
})
test('OCR cancellation before continuation or late reply never retries', async () => {
  const stop=new AbortController();let calls=0
  const first=createHarnessModule({async request(){calls++;stop.abort();return native({status:'error',can_fallback:true,next_ticket:'next-grant'})}},owner)
  await assert.rejects(first.run(params,stop.signal),/cancelled/);assert.equal(calls,1);await first.dispose()
  const abort=new AbortController();let finish;calls=0
  const second=createHarnessModule({request(){calls++;return calls===1?Promise.resolve(native({status:'unavailable',can_fallback:true,next_ticket:'next-grant'})):new Promise(resolve=>{finish=resolve})}},owner)
  const pending=second.run(params,abort.signal)
  while(!finish) await Promise.resolve()
  abort.abort();await assert.rejects(pending,/cancelled/)
  finish(tess({}));assert.equal(calls,2);await second.dispose()
})
test('Core or Host rejection never attempts another backend or grant', async () => {
  let calls=0
  const runner=createHarnessModule({async request(){calls++;throw new Error('Core authority withdrawn')}},owner)
  await assert.rejects(runner.run(params,new AbortController().signal),/withdrawn/)
  assert.equal(calls,1);await runner.dispose()
})
