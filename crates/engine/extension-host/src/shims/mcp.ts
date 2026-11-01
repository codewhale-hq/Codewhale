/** Definition proposals only. Rust owns the sole MCP catalog, transport and auth. */
import { Service, type Context } from '@deepseek-ai/cordis'
import type { RpcPeer } from '../rpc.ts'
import { isJson } from '../json.ts'
import { OwnedRegistrations, type OwnedEntry, type OwnerBase } from './owned.ts'
export interface McpDefinition { readonly serverName: string; readonly server: Record<string, unknown> }
export interface LocalMcp<O extends OwnerBase = OwnerBase> extends OwnedEntry<O> {}
export function normalizeMcp(value: unknown): McpDefinition {
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(k=>k!=='serverName'&&k!=='server')) throw new TypeError('MCP needs serverName and server')
  const {serverName,server}=value as McpDefinition
  if (typeof serverName!=='string'||!/^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$/.test(serverName)||!server||Array.isArray(server)||typeof server!=='object'||!isJson(server)) throw new TypeError('MCP definition must be bounded plain JSON')
  // Literal credential-bearing fields must never enter the host protocol.
  for(const key of ['headers','env']) if(server[key]!==undefined && (!server[key]||typeof server[key]!=='object'||Array.isArray(server[key])||Object.keys(server[key] as object).length)) throw new TypeError('MCP credentials remain under Rust authentication authority')
  if(typeof server.url==='string') {
    const url=new URL(server.url)
    if(url.username||url.password||url.search||url.hash) throw new TypeError('MCP endpoint credentials and query data remain under Rust authority')
  }
  if(Buffer.byteLength(JSON.stringify(server))>64*1024) throw new RangeError('MCP definition exceeds 64 KiB')
  return Object.freeze({serverName,server:structuredClone(server)})
}
export class McpDefinitions<O extends OwnerBase> {
  private readonly registrations:OwnedRegistrations<O,LocalMcp<O>>
  private readonly owners=new Map<O,Map<string,()=>void>>()
  constructor(rpc:RpcPeer,ownedBy:(owner:O)=>Map<number,LocalMcp<O>>,warn:(message:string,owner:O)=>void) { this.registrations=new OwnedRegistrations(rpc,'mcp_server',ownedBy,warn) }
  register(owner:O,value:McpDefinition):()=>void {
    const {serverName,server}=normalizeMcp(value)
    const definitions=this.owners.get(owner)??new Map<string,()=>void>()
    if(definitions.has(serverName)) throw new Error('MCP server is already registered in this entry; dispose it first')
    if(definitions.size>=64) throw new RangeError('MCP entry exceeds 64 servers')
    const undo=this.registrations.add({owner,name:serverName,disposed:false},{name:serverName,description:JSON.stringify(server)})
    const dispose=()=>{if(definitions.get(serverName)!==dispose)return;definitions.delete(serverName);if(!definitions.size)this.owners.delete(owner);undo()}
    definitions.set(serverName,dispose);this.owners.set(owner,definitions);return dispose
  }
  forget(owner:O) {for(const dispose of [...(this.owners.get(owner)?.values()??[])])dispose();this.registrations.forget(owner)}
}
export function defineMcpService<O extends OwnerBase>(host:{ownerOf(ctx:Context):O|undefined,definitions:McpDefinitions<O>}) {
  class McpShim extends Service {
    constructor(ctx:Context){super(ctx,'mcp')}
    registerServer(definition:McpDefinition):()=>void {
      const owner=host.ownerOf(this.ctx);if(!owner)throw new Error('MCP proposal is outside an extension owner')
      const normalized=normalizeMcp(definition)
      return this.ctx.effect(()=>host.definitions.register(owner,normalized),`mcp.registerServer(${JSON.stringify(normalized.serverName)})`)
    }
  }
  Object.freeze(McpShim.prototype);return McpShim
}
