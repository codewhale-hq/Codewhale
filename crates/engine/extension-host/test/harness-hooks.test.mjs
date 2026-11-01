import test from 'node:test'
import assert from 'node:assert/strict'
import {realpathSync,mkdtempSync,writeFileSync,mkdirSync,symlinkSync,rmSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {createHash} from 'node:crypto'
import {createHarnessModule} from '../dist/builtin/harness.mjs'
import {reviewedHookModule} from '../dist/shell-hooks.mjs'
const owner={plugin_id:'host:harness',generation:3,owner_token:'receipt'}
const all=['session_start','session_end','message_submit','tool_call_before','tool_call_after','mode_change','on_error','turn_end','subagent_spawn','subagent_complete','shell_env','session_idle','session_error','waiting_for_user','session_busy']
const metadata=(event='tool_call_before',dialect='claude-code',point='PreToolUse',more={})=>({event,dialect,point,query:'write',...more})
const params=(hook,more={})=>({owner,execution_id:'one',ticket:'opaque',deadline_ms:1000,hook,...more})
const output=(event,stdout='',exit_code=0,stderr='')=>({kind:'hook',event,success:exit_code===0,exit_code,stdout,stderr})
const signal=()=>new AbortController().signal
function fixture(raw){const root=realpathSync(mkdtempSync(join(tmpdir(),'cw-hook-bridge-')));const bytes=JSON.stringify(raw);writeFileSync(join(root,'hooks.json'),bytes);return {root,files:{'hooks.json':createHash('sha256').update(bytes).digest('hex')},cleanup:()=>rmSync(root,{recursive:true,force:true})}}
function context(){const rows=[];const warnings=[];return {rows,warnings,shellHooks:{register(row){rows.push(row);return ()=>{} }},logger:{warn(text){warnings.push(text)}}}}

test('all 15 existing core firepoint projections complete using one opaque Execution RPC',async()=>{
 for(const event of all){const calls=[];const module=createHarnessModule({async request(method,args){calls.push([method,args]);return {kind:'hook',event,success:true,exit_code:0,...(event==='shell_env'?{keys:['PATH','TOKEN']}:{})}}},owner)
 assert.deepEqual(await module.run(params(metadata(event,'codewhale',event)),signal()),{ok:true,hook_completed:true});assert.deepEqual(calls,[['exec/redeem',{owner,execution_id:'one',ticket:'opaque'}]]);await module.dispose()}
})
test('ShellEnv read-only projection refuses private stdout, stderr and unrelated process data',async()=>{
 for(const extra of [{stdout:'TOKEN=private'}, {stderr:'private'}, {environment:{TOKEN:'private'}}]){const module=createHarnessModule({async request(){return {kind:'hook',event:'shell_env',success:true,exit_code:0,keys:['TOKEN'],...extra}}},owner);await assert.rejects(module.run(params(metadata('shell_env','codewhale','shell_env')),signal()),/private process data/);await module.dispose()}
})
test('exact pinned Claude literal and Codex regex matcher differences govern actual redemption',async()=>{
 let calls=0;const module=createHarnessModule({async request(){calls++;return output('tool_call_before','{}')}},owner)
 assert.deepEqual(await module.run(params(metadata('tool_call_before','claude-code','PreToolUse',{matcher:'write',query:'write_file'})),signal()),{ok:true,hook_skipped:true});assert.equal(calls,0)
 assert.equal((await module.run(params(metadata('tool_call_before','codex','PreToolUse',{matcher:'write',query:'write_file'})),signal())).hook_completed,true);assert.equal(calls,1);await module.dispose()
})
test('hookSpecificOutput discriminator fences permission proposals and allow never approves',async()=>{
 let stdout=JSON.stringify({hookSpecificOutput:{hookEventName:'PostToolUse',permissionDecision:'deny'}});const module=createHarnessModule({async request(){return output('tool_call_before',stdout)}},owner)
 assert.deepEqual(JSON.parse((await module.run(params(metadata()),signal())).proposal),{})
 stdout=JSON.stringify({hookSpecificOutput:{hookEventName:'PreToolUse',permissionDecision:'allow'}});assert.deepEqual(JSON.parse((await module.run(params(metadata()),signal())).proposal),{});await module.dispose()
})
test('Claude ask maps to Rust proposal while Codex ignores ask; exit2 block is retained',async()=>{
 let exit=0;let stdout=JSON.stringify({hookSpecificOutput:{hookEventName:'PreToolUse',permissionDecision:'ask',permissionDecisionReason:'confirm'}});const module=createHarnessModule({async request(){return output('tool_call_before',stdout,exit,'reason')}},owner)
 assert.deepEqual(JSON.parse((await module.run(params(metadata()),signal())).proposal),{decision:'ask',reason:'confirm'})
 assert.deepEqual(JSON.parse((await module.run(params(metadata('tool_call_before','codex')),signal())).proposal),{})
 exit=2;stdout='';assert.deepEqual(JSON.parse((await module.run(params(metadata()),signal())).proposal),{decision:'deny',reason:'reason'});await module.dispose()
})
test('prompt block is a bounded submit proposal without falsifying process exit',async()=>{
 const module=createHarnessModule({async request(){return output('message_submit',JSON.stringify({decision:'block',reason:'blocked'}))}},owner)
 assert.deepEqual(JSON.parse((await module.run(params(metadata('message_submit','claude-code','UserPromptSubmit')),signal())).proposal),{block:true,reason:'blocked'});await module.dispose()
})
test('observer steering is explicitly unsupported and malformed/oversized output refuses',async()=>{
 for(const stdout of [JSON.stringify({continue:false}),JSON.stringify({decision:'block'}),'x'.repeat(65537)]){const module=createHarnessModule({async request(){return output('turn_end',stdout)}},owner);await assert.rejects(module.run(params(metadata('turn_end','claude-code','Stop')),signal()),/unavailable|invalid dialect/);await module.dispose()}
})
test('hook deadline cancellation and disposal settle an unresponsive broker without fallback',async()=>{
 const module=createHarnessModule({request(){return new Promise(()=>{})}},owner);await assert.rejects(module.run(params(metadata(),{deadline_ms:5}),signal()),/cancelled/)
 const pending=module.run(params(metadata()),signal());const rejection=assert.rejects(pending,/cancelled/);await module.dispose();await rejection
})
test('reviewed Claude config uses pinned parser and creates real core catalog contribution definitions',()=>{
 const f=fixture({hooks:{PreToolUse:[{matcher:'write|edit',hooks:[{type:'command',command:'${CLAUDE_PLUGIN_ROOT}/check',timeout:3}]}],SubagentStart:[{hooks:[{command:'true'}]}],Stop:[{hooks:[{command:'true'}]}]}})
 try{const ctx=context();reviewedHookModule('claude-code',f.root,f.files).apply(ctx,{configPath:'hooks.json'});assert.equal(ctx.rows.length,3);assert.equal(ctx.rows[0].hook.command,`${f.root}/check`);assert.equal(ctx.rows[0].matcher,'write|edit');assert.equal(ctx.rows[0].hook.timeout_secs,3);assert.equal(ctx.rows[1].hook.event,'turn_end');assert.equal(ctx.warnings.length,1);assert.equal(ctx.rows[2].hook.event,'subagent_spawn')}finally{f.cleanup()}
})
test('changed bytes, external path, symlink and unsupported async/noncommand config never register',()=>{
 for(const dialect of ['claude-code','codex']){const f=fixture({hooks:{PreToolUse:[{hooks:[{command:'true',...(dialect==='codex'?{async:true}:{type:'prompt'})}]}]}});try{const ctx=context();assert.throws(()=>reviewedHookModule(dialect,f.root,f.files).apply(ctx,{configPath:'hooks.json'}),/unsupported/);assert.equal(ctx.rows.length,0);writeFileSync(join(f.root,'hooks.json'),'{}');assert.throws(()=>reviewedHookModule(dialect,f.root,f.files).apply(ctx,{configPath:'hooks.json'}),/changed/);assert.throws(()=>reviewedHookModule(dialect,f.root,f.files).apply(ctx,{configPath:'../elsewhere.json'}),/closure/)}finally{f.cleanup()}}
 const f=fixture({hooks:{PreToolUse:[{hooks:[{command:'true'}]}]}});try{symlinkSync(join(f.root,'hooks.json'),join(f.root,'link.json'));const files={...f.files,'link.json':f.files['hooks.json']};assert.throws(()=>reviewedHookModule('claude-code',f.root,files).apply(context(),{configPath:'link.json'}),/closure/)}finally{f.cleanup()}
})
