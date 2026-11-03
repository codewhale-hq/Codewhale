import test from 'node:test'
import assert from 'node:assert/strict'
import { dirname, join } from 'node:path'
import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { transformStockSnapshot } from '../dist/stock-adapters.mjs'
import { createHarnessModule } from '../dist/builtin/harness.mjs'
import { startHost, BUNDLE } from './harness.mjs'
const transform = input => transformStockSnapshot({kind:'stock_adapter',operation:'github_result',input})
const fields = {title:'Lost tool result',expected:'Result reaches the agent',actual:'Result was missing',steps:['Request the result'],observed:['The result was absent'],inferred:[],impact:'Task needs a retry',reported_provider:null,reported_tool:'Run',reported_terminal:null,related_issues:[12,19]}
const report = {id:'cwreport_'+'a'.repeat(64),revises:null,fields,version:'0.10.1',platform:'linux x86_64',model:'test-model',redactions:['absolute_path','secret']}
const parse = input => JSON.parse(transform(input).result.content)
const owner = {plugin_id:'host:harness',generation:4,owner_token:'fixture-owned-job'}
const params = (id='one') => ({owner,execution_id:id,ticket:'opaque-core-execution-grant',deadline_ms:1000})

test('issue and PR snapshots preserve admitted source metadata and exact decimal caller number',()=>{
  const raw={number:42,title:'Title',comments:[{body:'CJK 漢字'}],body:'short',body_artifact:'untrusted-source-field'}
  for(const action of ['issue_context','pr_context']) {
    const got=parse({action,number:'18446744073709551615',raw,large_body:false,diff:null})
    assert.deepEqual(got[action==='issue_context'?'issue':'pr'],raw)
    assert.equal(got.summary,`${action==='issue_context'?'Issue':'PR'} #18446744073709551615: Title`)
    assert.equal(transform({action,number:'1',raw,large_body:false,diff:null}).result.metadata,null)
  }
})
test('large bodies and optional diffs use scalar controls and supplied Core refs without making metadata',()=>{
  const got=parse({action:'pr_context',number:'2',raw:{title:'T',body:'漢'.repeat(1197)+'END'},large_body:true,body_artifact:'artifacts/core-body',diff:'😀'.repeat(897)+'END',diff_artifact:'artifacts/core-diff'}).pr
  assert.equal(got.body_summary,'漢'.repeat(897)+'...');assert.equal(got.body,'漢'.repeat(1197)+'...')
  assert.equal(got.diff_summary,'😀'.repeat(897)+'...');assert.equal(got.body_artifact,'artifacts/core-body');assert.equal(got.diff_artifact,'artifacts/core-diff')
  const control=parse({action:'issue_context',number:'1',raw:{body:'\u0000a\u0085b\n\tc'},large_body:true,body_artifact:null,diff:null}).issue
  assert.equal(control.body_summary,'ab\n\tc')
})
test('every write and dry/dirty acknowledgement preserves prior target wording and success',()=>{
  for(const target of ['issue','pr']) {
    const subject=target==='issue'?'issue':'PR'
    for(const dry_run of [false,true]) {
      assert.equal(transform({action:'comment',target,number:'8',dry_run}).result.content,dry_run?`Dry run: would comment on ${target} #8.`:`Commented on ${target} #8.`)
      const action=target==='issue'?'close_issue':'close_pr'
      assert.equal(transform({action,target,number:'8',dry_run}).result.content,dry_run?`Dry run: would close ${subject} #8.`:`Closed ${subject} #8.`)
      const dirty=transform({action,target,number:'8',dirty:true}).result;assert.equal(dirty.success,false);assert.equal(dirty.content,`Refusing to close ${subject}: worktree is dirty and allow_dirty was false.`)
    }
  }
})
test('draft/read/operator review share one complete Markdown renderer and unavailable publication status',()=>{
  const draft=parse({action:'report_draft',report}),read=parse({action:'report_read',report}),review=transform({action:'report_review',report}).result.content
  assert.deepEqual(draft,read);assert.equal(draft.review,review);assert.equal(draft.publication,'unavailable');assert.equal(draft.duplicate_search,'not_performed')
  assert.equal(draft.artifact,`artifacts/issue-reports/${report.id}.json`)
  assert.ok(review.startsWith(`# Lost tool result\n\nDraft: ${report.id}\nStatus: ready for review\nPublication: unavailable`))
  assert.ok(review.includes('## Inferences (not verified)\n\nNone recorded.\n'));assert.ok(review.includes('- Provider (agent reported): unknown\n'))
  assert.ok(review.includes('- [#19](https://github.com/codewhale-hq/CodeWhale/issues/19)\n'));assert.ok(review.includes('Redacted categories: absolute_path, secret\n'))
  assert.ok(review.endsWith(`\nReview: \`/feedback review ${report.id}\`\nRevise: \`/feedback edit ${report.id} <change>\`\n`))
})
test('revision and inference fields remain attributed and distinct',()=>{
  const revised={...report,revises:'cwreport_'+'b'.repeat(64),fields:{...fields,inferred:['Perhaps a renderer failed'],reported_provider:'test-provider',related_issues:[]},redactions:[]}
  const content=transform({action:'report_review',report:revised}).result.content
  assert.ok(content.includes(`Revises: ${revised.revises}\n`));assert.ok(content.includes('## Inferences (not verified)\n\n- Perhaps a renderer failed\n'))
  assert.ok(!content.includes('Related issues'));assert.ok(!content.includes('Redacted categories'))
})
test('unknown action/target, malformed report and oversized snapshots refuse without truncation',()=>{
  for(const input of [{action:'publish',number:'1',target:'issue'},{action:'comment',target:'pull',number:'1'},{action:'report_review',report:{...report,fields:{...fields,steps:4}}},{action:'issue_context',number:'1',raw:{body:'x'.repeat(1024*1024+1)}}]) assert.throws(()=>transform(input))
})
test('actual Node runner redeems only one opaque operation and presents the admitted report',async()=>{
  const calls=[];const runner=createHarnessModule({async request(method,args){calls.push([method,args]);return {kind:'stock_adapter',operation:'github_result',input:{action:'report_read',report}}}},owner)
  const got=await runner.run(params(),new AbortController().signal)
  assert.equal(JSON.parse(got.result.content).report_id,report.id);assert.deepEqual(calls,[['exec/redeem',{owner,execution_id:'one',ticket:'opaque-core-execution-grant'}]])
  await runner.dispose()
})
test('runner cancellation and owner disposal cannot present a late GitHub completion',async()=>{
  let finish;const runner=createHarnessModule({request(){return new Promise(resolve=>{finish=resolve})}},owner)
  const controller=new AbortController(),pending=runner.run(params(),controller.signal);controller.abort()
  await assert.rejects(pending,/cancelled/);finish({kind:'stock_adapter',operation:'github_result',input:{action:'report_read',report}});await runner.dispose()
  await assert.rejects(runner.run(params('two'),new AbortController().signal),/stale/)
})
test('real builtin Host process handles the adopted GitHub operation over the wire',async t=>{
  const host=await startHost({tier:'builtin'});t.after(()=>host.stop())
  const entry=join(dirname(BUNDLE),'builtin','harness.mjs'),sha256=createHash('sha256').update(readFileSync(entry)).digest('hex')
  assert.equal((await host.call('ext/activate',{owner,plugin_name:'Pinned GitHub formatter',entry:{path:entry,sha256},config:{}})).status,'ok')
  const redeemed=host.waitFor(message=>message.method==='exec/redeem')
  const result=host.call('harness/run',params('wire'))
  const message=await redeemed;assert.deepEqual(message.params,{owner,execution_id:'wire',ticket:'opaque-core-execution-grant'})
  host.send({jsonrpc:'2.0',id:message.id,result:{kind:'stock_adapter',operation:'github_result',input:{action:'report_read',report}}})
  assert.equal(JSON.parse((await result).result.content).review,transform({action:'report_review',report}).result.content)
  assert.deepEqual(await host.call('ext/deactivate',{owner}),{disposed:true,leaked:[]})
})

test('frozen Rust source counterpart and adopted presenter produce byte-identical report reviews',()=>{
  const fixture=JSON.parse(readFileSync(new URL('../../tests/fixtures/github-host-parity.json',import.meta.url),'utf8'))
  for(const {report,review} of fixture.cases) assert.equal(transform({action:'report_review',report}).result.content,review)
})
