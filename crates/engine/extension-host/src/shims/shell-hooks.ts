/** Owner/scope registration only. Rust owns command review, scheduling, processes and steering. */
import { Service } from '@deepseek-ai/cordis'
import type { OwnerBase, OwnedEntry, OwnedRegistrations } from './owned.ts'
export interface ShellDefinition {dialect:'claude-code'|'codex';point:string;matcher?:string;hook:{event:string;command:string;timeout_secs:number;background:boolean;continue_on_error:boolean;name:string}}
export interface LocalShellHook<O extends OwnerBase> extends OwnedEntry<O> {}
export function defineShellHooksService<O extends OwnerBase>(options:{ownerOf:(ctx:any)=>O|undefined;registrations:OwnedRegistrations<O,LocalShellHook<O>>}) {
 class ShellHooks extends Service {
  constructor(ctx:any){super(ctx,'shellHooks')}
  register(definition:ShellDefinition):()=>void {
   const ctx:any=this.ctx;const owner=options.ownerOf(ctx)
   if(!owner || owner.ref.plugin_id.startsWith('host:'))throw new Error('shell hooks require a reviewed Native owner')
   const text=JSON.stringify(definition)
   if(Buffer.byteLength(text)>64*1024)throw new Error('shell hook definition exceeds 64 KiB')
   return ctx.effect(()=>options.registrations.add({owner,name:definition.hook.name,disposed:false},{name:definition.hook.name,description:text}),'shellHooks.register')
  }
 }
 Object.freeze(ShellHooks.prototype);return ShellHooks
}
