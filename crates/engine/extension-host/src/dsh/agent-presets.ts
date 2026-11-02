import { PERSONA, normalizePersonaConfig } from './persona.ts'
/** Raw DSH roster preparation, over the existing reviewed file/entry receipt.
 * Discovery borrows the pinned package's algorithms. Mounts use the one Loader;
 * Rust alone selects caller/AgentProfile ancestry and writes session/settings.
 */
import { Service } from '@deepseek-ai/cordis'
import { createHash } from 'node:crypto'
import { resolve, relative, dirname, sep } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { discoverPresets, type DiscoveryIO } from './upstream/agent-presets/discovery.ts'
import { PRESET_ID, type AgentPreset, type PresetRoot } from './upstream/agent-presets/preset.ts'
import { classifyRowSpecifier } from './upstream/agent-presets/specifier.ts'
import { fileComposition, mountedCompositionRows, type AgentPresetCompositionRow } from './upstream/agent-presets/composition-inventory.ts'
import {isJsExpr} from './upstream/loader/src/index.ts'
import type { ReviewedComposition, ReviewedModule, CompositionMount } from './composition.ts'

export const AGENT_PRESETS = '@deepseek-ai/dsh-agent-presets'
const MAX_PRESETS = 64
const hash = (bytes: string) => createHash('sha256').update(bytes).digest('hex')
const SHA = /^[a-f0-9]{64}$/
function plain(path: string): boolean {
  return typeof path === 'string' && path.length > 0 && path.length <= 4096 && !/[\\:\u0000-\u001f\u007f]/u.test(path)
    && !path.startsWith('/') && path.split('/').every(p => p !== '' && p !== '.' && p !== '..')
}
function fail(message: string): never { throw new Error(`agent-presets: ${message}`) }

/** Exact package export lookup inside the admitted inventory, no upward walk. */
export function containedPackageModule(name: string, documents: ReadonlyMap<string, string>, files: Readonly<Record<string, string>>): ReviewedModule | undefined {
  if (!name || /[\\:\u0000-\u0020\u007f]/u.test(name)) return
  const parts = name.split('/')
  const n = name.startsWith('@') ? 2 : 1
  if (parts.length < n || parts.some(p => !p || p === '.' || p === '..')) return
  const pkg = parts.slice(0, n).join('/')
  const packageRoot = `node_modules/${pkg}`
  const source = documents.get(`${packageRoot}/package.json`)
  if (source === undefined) return
  let data: any
  try { data = JSON.parse(source) } catch { return }
  if (data.name !== pkg || data.type !== 'module') return
  const subpath = parts.length === n ? '.' : `./${parts.slice(n).join('/')}`
  const refused=Symbol('unsupported package target')
  function target(value: any): string | undefined | typeof refused {
    if (typeof value === 'string') return value
    if (!value || Array.isArray(value) || typeof value !== 'object') return refused
    for (const [condition, child] of Object.entries(value)) {
      if (['node', 'import', 'default'].includes(condition)) { const picked = target(child); if (picked !== undefined) return picked }
    }
  }
  let entry: string | undefined | typeof refused
  if (data.exports !== undefined) {
    const value = data.exports
    entry = value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).some(k => k.startsWith('.'))
      ? target(value[subpath]) : subpath === '.' ? target(value) : undefined
  } else if (subpath === '.') entry = typeof data.main === 'string' ? data.main : './index.js'
  else entry = subpath
  if (typeof entry!=='string' || !entry.startsWith('./') || !plain(entry.slice(2))) return
  const path = `${packageRoot}/${entry.slice(2)}`
  if (!/\.(mjs|js|mts)$/.test(path) || !SHA.test(files[path] ?? '')) return
  return { name, path, sha256: files[path] }
}

export interface PresetCatalogRow {
  id: string
  trust: 'system' | 'user'
  name?: string
  description?: string
  order?: number
  broken?: string
  is_default: boolean
  entry?: { path: string; sha256: string }
}
export interface PresetCatalog {
  version: 1
  default?: string
  mode_selection_enabled: boolean
  presets: PresetCatalogRow[]
  /** Pure file projection; conditional gates stay conditional until mounted. */
  inventory: Record<string, {path: string; rows: AgentPresetCompositionRow[]} | {path: string; broken: string}>
}
export interface PreparedPreset {
  metadata: PresetCatalogRow
  composition: ReviewedComposition
  /** Original source for read/inventory; never written by the host. */
  source: string
}
export interface PresetReviewInput {
  kind: 'agent-presets'
  composition: ReviewedComposition
  /** Exact documents only; other module bytes remain in composition.files. */
  documents: { path: string; sha256: string; source: string }[]
  directories: string[]
}
export interface PresetReview {
  catalog: PresetCatalog
  presets: PreparedPreset[]
}

/** Nonexecuting discovery: no FS calls, home expansion or ambient package lookup. */
export async function reviewAgentPresets(input: PresetReviewInput): Promise<PresetReview> {
  const { composition } = input
  const files = composition.files
  if (!files || Object.keys(files).length > 4096 || !Array.isArray(input.directories) || input.directories.length > 4096) fail('invalid source inventory')
  const base = resolve('codewhale-reviewed-agent-presets')
  const baseUrl = pathToFileURL(`${base}${sep}`).href
  function key(path: string): string {
    const inside = relative(base, path).split(sep).join('/')
    if (!plain(inside)) fail('path escapes the admitted source root')
    return inside
  }
  const documents = new Map<string, string>()
  let bytes = 0
  for (const doc of input.documents) {
    if (!plain(doc.path) || !SHA.test(doc.sha256) || files[doc.path] !== doc.sha256 || hash(doc.source) !== doc.sha256 || documents.has(doc.path)) fail('document changed or is outside the source receipt')
    bytes += Buffer.byteLength(doc.source)
    if (Buffer.byteLength(doc.source) > 1024 * 1024 || bytes > 4 * 1024 * 1024) fail('documents exceed their bound')
    documents.set(doc.path, doc.source)
  }
  const directories = new Set(input.directories)
  for (const path of directories) if (!plain(path)) fail('invalid source directory')
  for (const path of Object.keys(files)) if (!plain(path) || !SHA.test(files[path])) fail('invalid source file receipt')
  const invalid = new Set<string>()
  const parsed = new Map<string, any[]>()
  const io: DiscoveryIO = {
    async readFile(path) { const relative = key(path); if (invalid.has(relative)) throw new Error('composition exceeds its data/shape bound'); const text = documents.get(relative); if (text === undefined) throw new Error('document not admitted'); return text },
    async stat(path) { return { isFile: () => Object.hasOwn(files, key(path)) } },
    async readdir(path) {
      const prefix = `${key(path)}/`
      if (!directories.has(prefix.slice(0, -1))) { const e = new Error('missing root'); Object.assign(e, { code: 'ENOENT' }); throw e }
      return [...directories].filter(p => p.startsWith(prefix) && !p.slice(prefix.length).includes('/'))
        .map(p => ({ name: p.slice(prefix.length), isDirectory: () => true }))
    },
  }
  // Import here avoids an initialization cycle; reviewer never imports a row.
  const { reviewComposition, parsePresetComposition, compositionRowProblem, hasReviewedRowBridge } = await import('./composition.ts')
  for (const [path, source] of documents) {
    if (!path.endsWith('/agent.cordis.yml')) continue
    try { parsed.set(path, parsePresetComposition(source)) } catch { invalid.add(path) }
  }
  const top = reviewComposition(composition)
  const rosterRows: any[] = []
  function visit(rows: any[]) {
    for (const row of rows) {
      if (row.group === true || row.name === 'cordis:group' || row.name === '@deepseek-ai/cordis-plugin-group') visit(row.config ?? [])
      else if (row.name === AGENT_PRESETS) rosterRows.push(row)
    }
  }
  visit(top.entries)
  if (rosterRows.length !== 1) fail('exactly one raw roster row is required')
  const config = rosterRows[0].config ?? {}
  if (!config || Array.isArray(config) || typeof config !== 'object' || Object.keys(config).some(k => !['default', 'roots', 'includeShippedRoot', 'includeUserRoot'].includes(k))) fail('roster configuration must be literal upstream fields')
  for (const k of ['includeShippedRoot', 'includeUserRoot']) if (config[k] !== undefined && typeof config[k] !== 'boolean') fail('root gates must be literal booleans')
  if (config.default !== undefined && (typeof config.default !== 'string' || !PRESET_ID.test(config.default) || config.default.length > 64)) fail('invalid default id')
  const roots: PresetRoot[] = []
  if (config.includeShippedRoot !== false) {
    const candidates = [...documents.entries()].filter(([p, text]) => {
      if (p !== 'package.json' && !p.endsWith('/package.json')) return false
      try { return JSON.parse(text).name === AGENT_PRESETS } catch { return false }
    })
    if (candidates.length !== 1) fail('shipped root requires exactly one admitted agent-presets package')
    const folder = dirname(candidates[0][0]).split(sep).join('/')
    const root = folder === '.' ? 'presets' : `${folder}/presets`
    if (!directories.has(root)) fail('shipped preset root was not admitted')
    roots.push({ path: resolve(base, root), trust: 'system' })
  }
  if (config.roots !== undefined && !Array.isArray(config.roots)) fail('roots must be a literal list')
  for (const root of config.roots ?? []) {
    if (!root || Object.keys(root).some(k => !['path', 'trust'].includes(k)) || !plain(root.path) || !['system', 'user'].includes(root.trust)) fail('configured roots must stay inside the admitted package')
    if (!directories.has(root.path)) fail('configured preset root was not admitted')
    roots.push({ path: resolve(base, root.path), trust: root.trust })
  }
  if (config.includeUserRoot !== false) {
    if (!directories.has('.agent-presets')) fail('user root requires an admitted .agent-presets tree; ambient home discovery is refused')
    roots.push({ path: resolve(base, '.agent-presets'), trust: 'user' })
  }
  if (roots.length > 64) fail('too many roots')
  const declared = new Map(composition.modules.map(m => [m.name, m]))
  if (declared.size !== composition.modules.length) fail('duplicate module name receipt')
  const found = await discoverPresets(roots, baseUrl, name => hasReviewedRowBridge(name) || declared.has(name) || containedPackageModule(name, documents, files) !== undefined, io)
  if (found.length > MAX_PRESETS) fail('too many presets')
  const prepared: PreparedPreset[] = []
  const rows: PresetCatalogRow[] = []
  const inventory: PresetCatalog['inventory'] = Object.create(null)
  for (const preset of found) {
    if (preset.id.length > 64 || (preset.name?.length ?? 0) > 1024 || (preset.description?.length ?? 0) > 4096) fail('preset metadata exceeds its bound')
    const metadata: PresetCatalogRow = { id: preset.id, trust: preset.trust, is_default: preset.id === config.default,
      ...(preset.name === undefined ? {} : { name: preset.name }), ...(preset.description === undefined ? {} : { description: preset.description }),
      ...(preset.order === undefined ? {} : { order: preset.order }), ...(preset.broken === undefined ? {} : { broken: preset.broken }) }
    rows.push(metadata)
    const path = key(preset.path)
    inventory[preset.id] = {path, ...(preset.broken !== undefined ? {broken:preset.broken} : await fileComposition(preset.path, () => { throw new Error('conditional until mounted') }, io.readFile))}
    if (preset.broken !== undefined) continue
    const source = await io.readFile(preset.path, 'utf8')
    const entries = parsed.get(key(preset.path))!
    const modules = new Map<string, ReviewedModule>()
    function resolveRows(children: any[]) {
      for (const row of children) {
        if (Boolean(row.disabled) && !isJsExpr(row.disabled)) continue
        if (row.group === true || row.name === 'cordis:group' || row.name === '@deepseek-ai/cordis-plugin-group') { resolveRows(row.config ?? []); continue }
        const specifier = classifyRowSpecifier(row.name)
        if (row.name === PERSONA) normalizePersonaConfig(row.config)
        if (specifier.kind === 'builtin' || hasReviewedRowBridge(row.name)) continue
        if (row.name === AGENT_PRESETS) fail('nested roster cannot create a second selection authority')
        const problem=compositionRowProblem(row.name);if(problem) fail(problem)
        let receipt: ReviewedModule | undefined
        if (specifier.kind === 'package') receipt = declared.get(row.name) ?? containedPackageModule(row.name, documents, files)
        else {
          const url = specifier.kind === 'file' ? new URL(specifier.specifier) : new URL(specifier.specifier, pathToFileURL(preset.path).href)
          const path = key(fileURLToPath(url))
          if (files[path] !== undefined) receipt = { name: row.name, path, sha256: files[path] }
        }
        if (!receipt || files[receipt.path] !== receipt.sha256) { if (isJsExpr(row.disabled)) continue; fail('row module has no exact admitted receipt') }
        modules.set(row.name, receipt)
      }
    }
    try { resolveRows(entries) } catch (error) { metadata.broken = error instanceof Error ? error.message : 'preset preparation refused'; continue }
    const layer = JSON.stringify([{ insert: entries }])
    const spec: ReviewedComposition = { version: 1, layers: [{ path: key(preset.path), sha256: hash(layer), source: layer }], modules: [...modules.values()], files }
    reviewComposition(spec)
    prepared.push({ metadata, composition: spec, source })
  }
  if (config.default !== undefined && !prepared.some(p => p.metadata.id === config.default)) fail('default preset is missing or broken')
  if (!prepared.length) fail('no admitted usable preset')
  if (Buffer.byteLength(JSON.stringify(inventory)) > 4 * 1024 * 1024) fail('composition inventory exceeds its bound')
  return { catalog: { version: 1, ...(config.default === undefined ? {} : {default:config.default}), mode_selection_enabled: true, presets: rows, inventory }, presets: prepared }
}

export interface PresetMountConfig {
  catalog: PresetCatalog
  selected: PreparedPreset
  base_url: string
}
/** The raw roster row is a scope-local view of Core's captured selected entry.
 * Its subtree owns the real five contributions. It cannot select/reparent an
 * Agent, start a turn, write a session, or mutate roots/settings.
 */
export function defineReviewedAgentPresets(mount: (ctx: any, base: string, spec: ReviewedComposition) => Promise<CompositionMount>) {
  return class ReviewedAgentPresets extends Service {
    static inject = ['loader']
    private readonly snapshot: PresetMountConfig
    private readonly selfCtx: any
    constructor(ctx: any, config: { codewhale_reviewed?: PresetMountConfig }) {
      super(ctx, 'agentPresets')
      this.selfCtx = ctx
      const snapshot = config?.codewhale_reviewed
      if (!snapshot || snapshot.catalog?.version !== 1 || !snapshot.catalog.presets.some(p => p.id === snapshot.selected?.metadata.id && !p.broken)) fail('raw roster needs an installer-minted preset receipt')
      this.snapshot = structuredClone(snapshot)
    }
    private mounting?: Promise<CompositionMount>
    private mounted?: CompositionMount
    mountSelected() { return this.mounting ??= mount(this.selfCtx, this.snapshot.base_url, this.snapshot.selected.composition).then(result => this.mounted=result) }
    async list() { return this.snapshot.catalog.presets.map(row => ({...structuredClone(row),path:fileURLToPath(new URL(this.snapshot.catalog.inventory[row.id].path,this.snapshot.base_url))})) }
    async remoteExportList() { return {presets:this.snapshot.catalog.presets.map(({is_default,entry,...row})=>({...structuredClone(row),isDefault:is_default})),authorable:false,defaultId:this.defaultId,modeSelectionEnabled:this.snapshot.catalog.mode_selection_enabled} }
    get defaultId() { return this.snapshot.catalog.default }
    get authorable() { return false }
    async resolve(id = this.defaultId) {
      const row = (await this.list()).find(p => p.id === id)
      if (!row) fail('preset not found')
      return row
    }
    composedPreset() { return this.snapshot.selected.metadata.id }
    async compositionInventory() {
      return this.snapshot.catalog.presets.map(row => {
        const identity={id:row.id,trust:row.trust,...(row.name===undefined?{}:{name:row.name}),isDefault:row.id===this.defaultId}
        if (row.id===this.composedPreset() && this.mounted) return {...identity,rows:mountedCompositionRows(this.mounted.tree)}
        const read=this.snapshot.catalog.inventory[row.id]
        if (row.broken || 'broken' in read) return {...identity,broken:row.broken ?? ('broken' in read?read.broken:undefined),rows:[]}
        return {...identity,rows:structuredClone(read.rows)}
      })
    }
    async readDocument(id: string) {
      if (id !== this.composedPreset()) fail('read another preset through Core file authority')
      return { agentPreset: id, trust: this.snapshot.selected.metadata.trust, content: this.snapshot.selected.source }
    }
    select() { fail('select through Core AgentProfile/native preset selection before a turn') }
    recompose() { fail('Core owns the captured agent selection and blank-session check') }
    copy() { fail('copy through Core file/install/review/reload authority') }
    remove() { fail('delete through Core file/install/review/reload authority') }
  }
}
