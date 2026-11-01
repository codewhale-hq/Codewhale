/** Pinned configuration/matcher semantics adapted onto the existing core hook catalog. No agent/session runtime. */
import { createHash } from 'node:crypto'
import { readFileSync,lstatSync } from 'node:fs'
import { resolve,relative,isAbsolute,sep } from 'node:path'
import { isUnlinkedInside } from './canonical-path.ts'
// Re-exported for the pure path-normalization unit test (test/canonical-path.test.mjs).
export { insideKey, pathKey, samePath, stripVerbatim, unlinkedKeyMatches } from './canonical-path.ts'
import { parseClaudeCodeConfig } from './upstream/hooks/hooks-claude-code/src/config.ts'
import { parseCodexConfig } from './upstream/hooks/hooks-codex/src/config.ts'
const EVENTS:Record<string,string>={SessionStart:'session_start',UserPromptSubmit:'message_submit',PreToolUse:'tool_call_before',PostToolUse:'tool_call_after',Stop:'turn_end',SubagentStart:'subagent_spawn',SubagentStop:'subagent_complete'}
export function reviewedHookModule(dialect:'claude-code'|'codex',root:string,files:Readonly<Record<string,string>>) {
 return {name:`hooks-${dialect}`,inject:['shellHooks'],apply(ctx:any,config:any) {
  if(!config || typeof config.configPath!=='string')throw new Error('hook bridge needs its reviewed configPath')
  const path=resolve(root,config.configPath);const inside=relative(root,path).split(sep).join('/')
  if(!inside || inside.startsWith('../') || isAbsolute(inside) || !files[inside] || !isUnlinkedInside(root,inside,path) || !lstatSync(path).isFile())throw new Error('hook config is absent from the reviewed regular-file closure')
  const bytes=readFileSync(path)
  if(bytes.length>1024*1024 || createHash('sha256').update(bytes).digest('hex')!==files[inside])throw new Error('hook config changed after review or exceeds 1 MiB')
  // The only substitutions are the sealed bundle and current per-call core workspace.
  // Project-dir values cannot name another ambient project during activation.
  if(config.projectDir!==undefined)throw new Error('explicit projectDir is unsupported; each process uses its current core workspace')
  if(config.pluginRoot!==undefined && config.pluginRoot!=='.' && config.pluginRoot!==root)throw new Error('pluginRoot must be this reviewed bundle root')
  const raw:unknown=JSON.parse(bytes.toString('utf8'))
  const parsed=dialect==='claude-code'?parseClaudeCodeConfig(raw,{pluginRoot:root}):parseCodexConfig(raw)
  if(parsed.skipped.length)throw new Error('hook config contains unsupported non-command or asynchronous hooks')
  let count=0
  for(const [point,groups] of Object.entries(parsed.config)) {
   if(point==='Stop')ctx.logger.warn('Stop runs at the core TurnEnd observer; forced continuation is unsupported')
   for(const group of groups)for(const hook of group.hooks) {
    if(++count>128)throw new Error('hook bridge exceeds 128 commands')
    const seconds=hook.timeoutSec ?? ((config.defaultTimeoutMs ?? 600000)/1000)
    if(!Number.isSafeInteger(seconds) || seconds<1 || seconds>86400)throw new Error('hook timeout must be 1–86400 whole seconds')
    // Per-event payload and environment are produced in Rust from the actual caller.
    ctx.shellHooks.register({dialect,point,...(group.matcher===undefined?{}:{matcher:group.matcher}),hook:{event:EVENTS[point],command:hook.command,timeout_secs:seconds,background:false,continue_on_error:false,name:`${dialect}:${point}:${count}`}})
   }
  }
  if(count===0)throw new Error('reviewed hook config has no supported command hooks')
 }}
}
