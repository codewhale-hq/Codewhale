/**
 * Module resolution for plugin code: one Cordis, one schemastery, one cosmokit.
 *
 * DSH packages import these by bare specifier, as peers or (for schemastery,
 * sometimes) as plain dependencies. Two copies of Cordis break `instanceof`,
 * symbols and services, so every import of these specifiers — however the
 * package declared it, and ignoring any copy under the package's own
 * `node_modules` — resolves to the single instance bundled into this host.
 *
 * `@deepseek-ai/dsh-tools` resolves to the definition-side compat module, and
 * `@deepseek-ai/dsh-commands/brand` (the `CommandDefinitionId` constructor DSH
 * command plugins import) to its two identity functions. Any other `@deepseek-ai/dsh-*` package fails the import loudly: the host
 * does not provide it, and a silent partial load would be worse.
 *
 * Node: `module.registerHooks`. Bun has no `registerHooks` (a named import of
 * it fails at link time, so it is read off the namespace). Bun gets the same
 * rules through `Bun.plugin`, with three pieces (see `installBunResolver`).
 */
import * as nodeModule from 'node:module'
import {createHash} from 'node:crypto'
import {fileURLToPath,pathToFileURL} from 'node:url'
import {resolve,isAbsolute,dirname} from 'node:path'
import {readFileSync} from 'node:fs'
import { canonicalPath, insideKey, pathKey, unlinkedKeyMatches } from './canonical-path.ts'
import { RUNTIME } from '../runtime.ts'
import { prepareBunSource, REVIEWED_IMPORT } from './bun-closure.ts'
import compatibility from './compatibility.json' with { type: 'json' }

const SCHEME = 'codewhale-host:'
const REGISTRY_KEY = Symbol.for('codewhale.extension-host.modules')

/** Specifier → singleton key. */
const SINGLETONS: Readonly<Record<string, string>> = compatibility.module_singletons

export class UnsupportedPeerError extends Error {
  constructor(readonly specifier: string) {
    super(`requires \`${specifier}\`, which the Codewhale extension host does not provide`)
    this.name = 'UnsupportedPeerError'
  }
}

function packageName(specifier: string): string {
  const parts = specifier.split('/')
  return specifier.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0]
}

/** Map a bare specifier to a singleton key, `null` for "not ours", or throw for an unsupported DSH peer. */
export function classifySpecifier(specifier: string): string | null {
  if (specifier.startsWith('.') || specifier.startsWith('/') || specifier.includes(':')) return null
  if (Object.hasOwn(SINGLETONS, specifier)) return SINGLETONS[specifier]
  const name = packageName(specifier)
  if (Object.hasOwn(SINGLETONS, name)) {
    if (specifier !== name) throw new UnsupportedPeerError(specifier)
    return SINGLETONS[name]
  }
  if (name.startsWith('@deepseek-ai/dsh-') || name.startsWith('@deepseek-ai/cordis-')) {
    throw new UnsupportedPeerError(name)
  }
  return null
}

function virtualSource(key: string, namespace: Record<string, unknown>): string {
  const lines = [`const ns = globalThis[Symbol.for(${JSON.stringify(REGISTRY_KEY.description)})][${JSON.stringify(key)}];`]
  for (const name of Object.keys(namespace)) {
    if (name === 'default') continue
    if (!/^[A-Za-z_$][\w$]*$/.test(name)) continue
    lines.push(`export const ${name} = ns[${JSON.stringify(name)}];`)
  }
  if ('default' in namespace) lines.push('export default ns.default;')
  return lines.join('\n')
}

/**
 * A path inside a `node_modules` copy of a package the host owns or refuses:
 * the singletons and every `@deepseek-ai/dsh-*` / `@deepseek-ai/cordis-*` peer.
 */
const PEER_PATH =
  /[\\/]node_modules[\\/]((?:@deepseek-ai[\\/](?:dsh-[^\\/]+|cordis(?:-[^\\/]+)?|schemastery|cosmokit))|cosmokit)[\\/]/

/**
 * Bun reports a bare import it cannot find as `Cannot find package 'X'`,
 * where Node would have run the resolve hook and thrown `UnsupportedPeerError`.
 * This gives the same error for unsupported peers. Other errors pass through.
 */
export function explainImportError(error: unknown): unknown {
  const match = error instanceof Error ? /Cannot find package '([^']+)'/.exec(error.message) : null
  if (!match) return error
  try {
    classifySpecifier(match[1])
  } catch (unsupported) {
    return unsupported
  }
  return error
}

/**
 * Bun 1.4: runtime `onResolve` is not called for bare package names, only for
 * some subpaths. So:
 * 1. `build.module` serves each singleton by its exact name.
 * 2. `onResolve` applies `classifySpecifier` to every bare specifier Bun does
 *    pass it, which catches subpaths such as `@deepseek-ai/cordis/lib/x`.
 * 3. `onLoad` refuses any file under a `node_modules` copy of a peer. A second
 *    Cordis, or a dsh peer that a plugin ships itself, never loads.
 *    `explainImportError` covers peers that are not installed at all.
 */
function installBunResolver(modules: Record<string, Record<string, unknown>>) {
  const bun = (globalThis as any).Bun
  Object.defineProperty(globalThis,REVIEWED_IMPORT,{value:async(specifier:unknown,caller:string,options?:unknown)=>{
    if(typeof specifier==='symbol')throw new TypeError('composition import specifier cannot be a symbol')
    const name=String(specifier)
    const closure=closureAt(caller)
    if(!closure)throw new Error('composition module closure is no longer admitted')
    const target=checkedBunSpecifier(name,closure.receipt,caller,false)
    const singleton=classifySpecifier(target)
    if(singleton!==null)return modules[singleton]
    return import(target,options as any)
  },writable:false,configurable:false})
  bun.plugin({
    name: 'codewhale-host-modules',
    setup(build: any) {
      for (const [specifier, key] of Object.entries(SINGLETONS)) {
        if (key in modules) build.module(specifier, () => ({ exports: modules[key], loader: 'object' }))
      }
      build.onResolve({ filter: /^[^./]/ }, (args: { path: string }) => {
        const key = classifySpecifier(args.path)
        if (key !== null && !(key in modules)) throw new UnsupportedPeerError(args.path)
        return undefined
      })
      build.onLoad({ filter: new RegExp(`${PEER_PATH.source}|\\.(?:mjs|js|cjs|mts|cts|ts|tsx|jsx|json)$`) }, (args: { path: string }) => {
        const match = PEER_PATH.exec(args.path)
        if(match)throw new UnsupportedPeerError(match[1].replaceAll('\\', '/'))
        const closure=closureAt(pathToFileURL(args.path).href)
        if(!closure) {
          const extension=args.path.split('.').at(-1)
          if(extension==='json')return {contents:`export default JSON.parse(${JSON.stringify(readFileSync(args.path,'utf8'))});`,loader:'js'}
          const loader=extension==='tsx'?'tsx'
            :extension==='ts' || extension==='mts' || extension==='cts'?'ts'
              :extension==='jsx' || extension==='js'?'jsx':'js'
          return {contents:readFileSync(args.path,'utf8'),loader}
        }
        if(!(closure.path in closure.receipt.files) || !unlinkedInside(closure.receipt,closure.path,args.path))throw new Error('module was absent from reviewed composition closure or contains a symbolic link')
        const bytes=readFileSync(args.path)
        if(bytes.length>64*1024*1024 || createHash('sha256').update(bytes).digest('hex')!==closure.receipt.files[closure.path])throw new Error('composition module bytes changed after review')
        // Bun 1.4 runtime onLoad treats its JSON loader as JS. Preserve exact
        // JSON semantics (including __proto__ data keys) via a JS data module.
        if(closure.path.endsWith('.json'))return {contents:`export default JSON.parse(${JSON.stringify(bytes.toString('utf8'))});`,loader:'js'}
        let source:string
        try {source=prepareBunSource(bytes.toString('utf8'),args.path,(specifier,require)=>checkedBunSpecifier(specifier,closure.receipt,pathToFileURL(args.path).href,require))}
        catch(error){if(error instanceof SyntaxError)throw new Error('reviewed composition module has unsupported JavaScript syntax');throw error}
        return {contents:source,loader:'js'}
      })
    },
  })
}

function checkedBunSpecifier(specifier:string,closure:ReviewedClosure,caller:string,require:boolean):string {
  const key=classifySpecifier(specifier)
  if(key!==null) {
    if(!(key in (globalThis as any)[REGISTRY_KEY]))throw new UnsupportedPeerError(specifier)
    return specifier
  }
  if(nodeModule.isBuiltin(specifier))return specifier.startsWith('node:')?specifier:`node:${specifier}`
  const file=(specifier.startsWith('file:') || specifier.startsWith('.') || isAbsolute(specifier))
    ?new URL(specifier,caller):undefined
  const path=file?(require && !specifier.startsWith('file:')?resolve(dirname(fileURLToPath(caller)),specifier):resolve(fileURLToPath(file))):undefined
  if(!path)throw new Error('bare dependency is absent from this reviewed composition; package its reviewed relative source')
  if(file?.search || file?.hash)throw new Error('Bun does not preserve reviewed module query or fragment identity; select [extension_host] runtime = \"node\" for this composition')
  const inside=keyIn(closure,path)
  if(inside===undefined || !(inside in closure.files))throw new Error('composition import escapes the reviewed file closure')
  return require?path:file!.href
}

// Module graph admission for an already-reviewed composition. This is an
// ephemeral resolver index over the existing tree's file receipt, not an owner,
// session or plugin state store. Native JS remains arbitrary co-resident code.
//
// Keyed by the canonical root's `pathKey`. A module path is matched lexically
// against the canonical root or a raw root it was admitted under — never a raw
// spelling against a canonical one (see canonical-path.ts): on Windows the two
// differ in prefix, 8.3 names and case for the very same directory.
interface ReviewedClosure {files:Readonly<Record<string,string>>,refs:number,canonicalRoot:string,rawRoots:Set<string>}
const reviewedClosures=new Map<string,ReviewedClosure>()
function keyIn(closure:ReviewedClosure,path:string):string|undefined {
  const canonical=insideKey(closure.canonicalRoot,path)
  if(canonical!==undefined)return canonical
  for(const root of closure.rawRoots) {
    const raw=insideKey(root,path)
    if(raw!==undefined)return raw
  }
}
/** `path` is `root/key` with no link inside the root (both sides canonicalized). */
function unlinkedInside(closure:ReviewedClosure,key:string,path:string):boolean {
  let canonical:string
  try {canonical=canonicalPath(path)} catch {return false}
  return unlinkedKeyMatches(closure.canonicalRoot,key,canonical)
}
export function admitReviewedClosure(baseUrl:string,files:Readonly<Record<string,string>>):()=>void {
  const rawRoot=resolve(fileURLToPath(baseUrl))
  const canonicalRoot=canonicalPath(rawRoot)
  const id=pathKey(canonicalRoot)
  const accepted:Record<string,string>=Object.create(null)
  const keys=Object.keys(files)
  if (keys.length>4096) throw new Error('reviewed composition closure exceeds its file limit')
  for (const key of keys) {
    if (!key || key.split('/').some(part=>!part || part==='.' || part==='..') || key.includes('\\') || key.includes(':') || isAbsolute(key) || !/^[a-f0-9]{64}$/.test(files[key])) throw new Error('invalid reviewed composition closure file')
    accepted[key]=files[key]
  }
  const existing=reviewedClosures.get(id)
  if(existing) {
    if(Object.keys(existing.files).length!==keys.length || keys.some(key=>existing.files[key]!==accepted[key])) throw new Error('the same composition root carries different file receipts')
    existing.refs++
    existing.rawRoots.add(rawRoot)
  } else reviewedClosures.set(id,{files:Object.freeze(accepted),refs:1,canonicalRoot,rawRoots:new Set([rawRoot])})
  let disposed=false
  return ()=>{if(disposed)return;disposed=true;const current=reviewedClosures.get(id);if(current && --current.refs===0)reviewedClosures.delete(id)}
}
/** Both runtimes consume the same exact admitted source-file receipt. */
export async function importReviewedModule(baseUrl:string,path:string):Promise<unknown> {
  const rawRoot=resolve(fileURLToPath(baseUrl))
  const id=pathKey(canonicalPath(rawRoot))
  const receipt=reviewedClosures.get(id)
  if(!receipt)throw new Error('composition module closure is no longer admitted')
  // The entry keeps the caller's spelling of the root: the one the composition
  // already read and hashed the module through.
  const entry=resolve(rawRoot,path)
  const admitted=closureAt(pathToFileURL(entry).href)
  if(!admitted || admitted.root!==id || !(admitted.path in receipt.files))throw new Error('composition module is absent from reviewed closure')
  return import(pathToFileURL(entry).href)
}
function closureAt(url:string|undefined) {
  if(!url?.startsWith('file:'))return
  const path=resolve(fileURLToPath(url))
  for(const [root,receipt] of reviewedClosures) {
    const inside=keyIn(receipt,path)
    if(inside!==undefined)return {root,receipt,path:inside}
  }
}

let installed = false

/**
 * Install the hooks once, publishing `modules` (key → module namespace) as the
 * singletons plugin code will see.
 */
export function installResolveHooks(modules: Record<string, Record<string, unknown>>) {
  if (installed) return
  installed = true
  ;(globalThis as any)[REGISTRY_KEY] = modules
  if (RUNTIME.name === 'bun') {
    installBunResolver(modules)
    return
  }
  nodeModule.registerHooks({
    resolve(specifier, context, nextResolve) {
      const key = classifySpecifier(specifier)
      if (key !== null) {
        if (!(key in modules)) throw new UnsupportedPeerError(specifier)
        return { url: `${SCHEME}${key}`, format: 'module', shortCircuit: true }
      }
      const caller=closureAt(context.parentURL)
      if(caller && !nodeModule.isBuiltin(specifier) && !specifier.startsWith('.') && !specifier.startsWith('/') && !specifier.startsWith('file:')) throw new Error('bare dependency is absent from this reviewed composition; package its reviewed relative source')
      const result=nextResolve(specifier, context)
      if(caller && result.url.startsWith('file:')) {
        const target=closureAt(result.url)
        if(!target || target.root!==caller.root || !(target.path in caller.receipt.files)) throw new Error('composition import escapes the reviewed file closure')
      }
      return result
    },
    load(url, context, nextLoad) {
      if (url.startsWith(SCHEME)) {
        const key = url.slice(SCHEME.length)
        return { format: 'module', source: virtualSource(key, modules[key]), shortCircuit: true }
      }
      const result=nextLoad(url, context)
      const closure=closureAt(url)
      if(closure) {
        if(!(closure.path in closure.receipt.files) || result.source===undefined || result.source===null) throw new Error('module was absent from reviewed composition closure')
        // Windows runs Node with --preserve-symlinks, so a linked module keeps
        // its in-closure URL; refuse it here as the Bun loader does.
        if(!unlinkedInside(closure.receipt,closure.path,fileURLToPath(url))) throw new Error('module was absent from reviewed composition closure or contains a symbolic link')
        const source=typeof result.source==='string'?Buffer.from(result.source):Buffer.from(result.source as Uint8Array)
        if(source.length>64*1024*1024 || createHash('sha256').update(source).digest('hex')!==closure.receipt.files[closure.path]) throw new Error('composition module bytes changed after review')
      }
      return result
    },
  })
}
