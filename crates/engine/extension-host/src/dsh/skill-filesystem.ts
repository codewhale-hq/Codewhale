/** The explicit reviewed-root subset of the raw DSH filesystem row. */
import type { Context } from '@deepseek-ai/cordis'
import { normalizeSkillRoot } from '../shims/skills.ts'
export interface ReviewedSkillContext extends Context { skills:{registerRoot(definition:{path:string}):()=>void} }
export function reviewedSkillModule() {
  return {name:'codewhale-reviewed-skills',inject:['skills'],apply(ctx:ReviewedSkillContext,config:Record<string,unknown>) {
    if(!config||Array.isArray(config)||Object.keys(config).some(k=>!['providerName','includeDefaultRoots','customSkillDirs','watch'].includes(k)))throw new Error('filesystem row requires reviewed roots; ambient roots and watcher controls are unsupported')
    if(config.includeDefaultRoots!==false||config.watch!==false)throw new Error('filesystem row must explicitly disable default roots and watchers')
    if(config.providerName!==undefined&&(typeof config.providerName!=='string'||!config.providerName||config.providerName.length>64))throw new Error('skill providerName must be bounded')
    if(!Array.isArray(config.customSkillDirs)||!config.customSkillDirs.length||config.customSkillDirs.length>8)throw new Error('filesystem row needs 1–8 reviewed roots')
    const roots=config.customSkillDirs.map(path=>normalizeSkillRoot({path}))
    const seen=new Set<string>();for(const {path} of roots){if(seen.has(path))throw new Error('duplicate filesystem root');seen.add(path)}
    for(const {path} of roots)ctx.skills.registerRoot({path:`source/${path}`})
  }}
}
