/**
 * Immutable, reviewed DSH entry trees in the existing
 * Cordis owner. Patch/expression/group/isolation semantics are upstream's.
 * There is no session writer, prompt builder, skill parser or process launcher.
 */
import { Context, Service, type Fiber } from '@deepseek-ai/cordis'
import { FiberState } from './fiber-state.ts'
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { isAbsolute, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Loader, EntryTree, Group, type EntryOptions } from './upstream/loader/src/index.ts'
import { applyEntryPatches, entryListSchema, type PatchOptions } from './upstream/include/src/index.ts'
import { load as loadYaml } from 'js-yaml'
import {reviewedMcpModule} from './mcp.ts'
import {reviewedSkillModule} from './skill-filesystem.ts'
import {reviewedHookModule} from './shell-hooks.ts'
import {admitReviewedClosure,importReviewedModule} from './resolve-hooks.ts'
import { PERSONA, reviewedPersonaModule } from './persona.ts'
import { AGENT_PRESETS, defineReviewedAgentPresets, type PresetCatalog, type PreparedPreset } from './agent-presets.ts'
import compatibility from './compatibility.json' with { type: 'json' }

const MAX_LAYERS = 64
const MAX_SOURCE_BYTES = 1024 * 1024
const MAX_DATA_BYTES = 4 * 1024 * 1024
const MAX_DATA_NODES = 100_000
const MAX_ROWS = 1024
const MAX_DEPTH = 32
const SHA256 = /^[a-f0-9]{64}$/

export interface ReviewedLayer {
  /** Original selected layer, for nonexecuting diagnostics. */
  path: string
  sha256: string
  /** Exact UTF-8 source; include's schema retains unevaluated !!js nodes. */
  source: string
}

export interface ReviewedModule {
  /** Exact row name; relative names are resolved during Rust staging. */
  name: string
  /** Path relative to the immutable Codewhale bundle root. */
  path: string
  sha256: string
}

export interface ReviewedComposition {
  version: 1
  layers: ReviewedLayer[]
  modules: ReviewedModule[]
  /** Original source/asset closure. Rust additionally hashes generated files. */
  files: Record<string, string>
}

export interface CompositionReview {
  version: 1
  /** Upstream's exact effective data. Expression nodes are still data. */
  entries: EntryOptions[]
  warnings: string[]
  skipped: {row?:string;package?:string;reason:string;layer:string;patch:number}[]
  layers: { path: string; sha256: string }[]
}

function digest(bytes: string | Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex')
}

function plainRelative(path: string): boolean {
  return typeof path === 'string' && path.length > 0 && path.length <= 4096 && !isAbsolute(path)
    && !path.startsWith('/') && !/[\\:\u0000-\u001f\u007f]/u.test(path)
    && path.split('/').every(part => part !== '' && part !== '.' && part !== '..')
}

/** Bound traversal before clone/stringification, including repeated YAML aliases. */
function boundData(root: unknown): void {
  let nodes = 0
  let bytes = 0
  const ancestors = new Set<object>()
  function visit(value: unknown, depth: number) {
    if (++nodes > MAX_DATA_NODES || depth > MAX_DEPTH) throw new Error('composition data exceeds its resource limit')
    if (typeof value === 'string') bytes += Buffer.byteLength(value)
    if (bytes > MAX_DATA_BYTES) throw new Error('composition expanded text exceeds 4 MiB')
    if (value === null || typeof value !== 'object') return
    if (ancestors.has(value)) throw new Error('cyclic configuration data is unsupported')
    ancestors.add(value)
    if (!Array.isArray(value) && Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) {
      throw new Error('composition configuration must be plain data')
    }
    for (const [key, child] of Object.entries(value)) {
      bytes += Buffer.byteLength(key)
      visit(child, depth + 1)
    }
    ancestors.delete(value)
  }
  visit(root, 0)
}

function assignmentSafe(value: object): void {
  // Patch/entry fields are assigned onto helper objects. Nested configuration
  // remains literal data; ordinary constructor/prototype config keys are valid.
  if (Object.hasOwn(value, '__proto__')) throw new Error('prototype-mutating entry field is unsupported')
}

function checkRows(rows: EntryOptions[], depth = 0, count = { value: 0 }): void {
  if (depth > MAX_DEPTH) throw new Error('composition entry nesting exceeds 32 levels')
  for (const row of rows) {
    if (++count.value > MAX_ROWS) throw new Error('composition exceeds 1024 entry rows')
    if (row === null || typeof row !== 'object' || Array.isArray(row)) throw new Error('entry row must be an object')
    assignmentSafe(row)
    if (row.id !== undefined && (typeof row.id !== 'string' || row.id.length > 512)) throw new Error('entry id must be a bounded string')
    if (!(row.group===true && row.name===undefined) && (typeof row.name !== 'string' || row.name.length === 0 || row.name.length > 4096)) throw new Error('entry name must be a bounded string')
    if ((row.group || row.name==='cordis:group' || row.name==='@deepseek-ai/cordis-plugin-group') && Array.isArray(row.config)) checkRows(row.config, depth + 1, count)
  }
}

/** Raw preset files are entry lists; apply the same bounded data/row guards
 * before the borrowed recursive discovery/inventory code sees them. */
export function parsePresetComposition(source: string): EntryOptions[] {
  if (Buffer.byteLength(source) > MAX_SOURCE_BYTES) throw new Error('preset source exceeds its limit')
  const rows = loadYaml(source, {schema:entryListSchema})
  boundData(rows)
  if (!Array.isArray(rows)) throw new Error('preset composition must be an entry list')
  checkRows(rows as EntryOptions[])
  return rows as EntryOptions[]
}

/** Trusted pure reviewer: never imports row modules or evaluates expressions. */
export function reviewComposition(spec: ReviewedComposition): CompositionReview {
  if (!spec || spec.version !== 1 || !Array.isArray(spec.layers) || spec.layers.length === 0 || spec.layers.length > MAX_LAYERS) {
    throw new Error('composition needs 1–64 reviewed layers')
  }
  let sourceBytes = 0
  const patches: PatchOptions[] = []
  const locations: {layer:string;patch:number}[] = []
  const layers: CompositionReview['layers'] = []
  for (const layer of spec.layers) {
    if (!layer || !plainRelative(layer.path) || typeof layer.source !== 'string' || typeof layer.sha256 !== 'string' || !SHA256.test(layer.sha256)) throw new Error('invalid reviewed layer')
    sourceBytes += Buffer.byteLength(layer.source)
    if (sourceBytes > MAX_SOURCE_BYTES || digest(layer.source) !== layer.sha256) throw new Error('layer source changed or exceeds 1 MiB')
    let parsed: unknown
    try { parsed = loadYaml(layer.source, { schema: entryListSchema }) } catch { throw new Error('invalid DSH patch data') }
    if (!Array.isArray(parsed)) throw new Error('DSH patch layer must be a list')
    boundData(parsed)
    for (const patch of parsed) {
      if (!patch || typeof patch !== 'object' || Array.isArray(patch)) throw new Error('patch must be an object')
      assignmentSafe(patch)
      if (patch.insert !== undefined && patch.insert !== null) {
        if (!Array.isArray(patch.insert)) throw new Error('patch insert must be a list')
        checkRows(patch.insert)
      }
    }
    patches.push(...parsed)
    locations.push(...parsed.map((_,i)=>({layer:layer.path,patch:i+1})))
    layers.push({ path: layer.path, sha256: layer.sha256 })
  }
  boundData(patches)
  const warnings: string[] = []
  const skipped: CompositionReview['skipped'] = []
  let entries: EntryOptions[] = []
  for (const [i,patch] of patches.entries()) {
    entries = applyEntryPatches(entries,[patch],(message,...args)=>{
      let index=0
      const reason=message.replace(/%C/g,()=>JSON.stringify(args[index++]))
      warnings.push(reason)
      skipped.push({...(typeof patch.id==='string'?{row:patch.id}:{}),...(typeof patch.name==='string'?{package:patch.name}:{}),reason,...locations[i]})
    })
  }
  boundData(entries)
  checkRows(entries)
  return { version: 1, entries, warnings, skipped, layers }
}

/** Known DSH rows that need an unavailable broker/service or replace core authority. */
const UNSUPPORTED_ROWS = new Map<string,string>()

/** These fixed Core adapters resolve without a package-provided module. */
export function hasReviewedRowBridge(name:string): boolean { return Object.hasOwn(compatibility.row_bridges, name) }

/** Readiness uses the same known Core service boundary as final import. */
export function compositionRowProblem(name:string): string | undefined { return UNSUPPORTED_ROWS.get(name) }

/** Upstream Group needs its existing Loader during child creation. Declare
 * that dependency for the host's strict Cordis service access checks. */
class ReviewedGroup extends Group { static inject = ['loader'] }

/** A Loader that uses upstream lifecycle/config logic without native internals. */
export class ReviewedLoader extends Loader {
  constructor(ctx: Context, config: Loader.Config) {
    super(ctx, config)
    this.builtins.group = ReviewedGroup
  }
}

// Loader installs global Cordis lifecycle observers. One host root must own
// exactly one loader; one loader per tree would duplicate interpolation.
const loaders = new WeakMap<Context, { ctx: Context; fiber: Fiber }>()

/** HostRoot calls this before any Native owner can activate. */
export function installCompositionLoader(root: Context): void {
  if (loaders.has(root)) return
  const ctx = root.isolate('loader')
  const fiber = ctx.plugin(ReviewedLoader, {})
  loaders.set(root, { ctx, fiber })
}

class ReviewedTree extends EntryTree {
  private readonly base: string
  private readonly modules: ReadonlyMap<string, ReviewedModule>
  private readonly spec: ReviewedComposition

  constructor(ctx: Context, base: string, spec: ReviewedComposition) {
    super(ctx)
    this.base = base
    this.spec = spec
    const modules = new Map<string, ReviewedModule>()
    if (!Array.isArray(spec.modules) || spec.modules.length > MAX_ROWS || !spec.files || Object.keys(spec.files).length > 4096) throw new Error('invalid reviewed module closure')
    for (const module of spec.modules) {
      if (!module || typeof module.name !== 'string' || module.name.length === 0 || module.name.length > 4096 || modules.has(module.name) || !plainRelative(module.path) || !SHA256.test(module.sha256)
        || spec.files[module.path] !== module.sha256) throw new Error('module map differs from reviewed closure')
      modules.set(module.name, module)
    }
    this.modules = modules
  }

  /** A reviewed composition is input; teardown and self-disposal never rewrite it. */
  write(): void {}

  override import(name: string): unknown {
    if (name === PERSONA) return reviewedPersonaModule
    if (name === AGENT_PRESETS) return defineReviewedAgentPresets(mountReviewedComposition)
    if (name === 'cordis:group' || name === '@deepseek-ai/cordis-plugin-group') return ReviewedGroup
    if(name==='@deepseek-ai/dsh-mcp-client')return reviewedMcpModule()
    if(name==='@deepseek-ai/dsh-skill-filesystem')return reviewedSkillModule()
    if(name==='@deepseek-ai/dsh-hooks-claude-code')return reviewedHookModule('claude-code',fileURLToPath(this.base),this.spec.files)
    if(name==='@deepseek-ai/dsh-hooks-codex')return reviewedHookModule('codex',fileURLToPath(this.base),this.spec.files)
    if (UNSUPPORTED_ROWS.has(name)) throw new Error(`${name}: ${UNSUPPORTED_ROWS.get(name)}`)
    const module = this.modules.get(name)
    if (!module) throw new Error(`row requires ${JSON.stringify(name)}, absent from the reviewed module closure`)
    return this.importReviewed(module)
  }

  private async importReviewed(module: ReviewedModule): Promise<unknown> {
    const root = fileURLToPath(this.base)
    const path = resolve(root, module.path)
    const inside = relative(root, path)
    if (inside.startsWith(`..${sep}`) || inside === '..' || isAbsolute(inside)) throw new Error('module escapes reviewed bundle')
    if (this.spec.files[module.path] !== module.sha256 || digest(await readFile(path)) !== module.sha256) throw new Error('module changed after review')
    // Transitive module bytes remain covered by the existing Rust Native
    // receipt, immutable staging and per-call liveness checks. Native code is
    // not contained from co-resident arbitrary Native code by this helper.
    return importReviewedModule(this.base,module.path)
  }
}

export interface CompositionMount {
  readonly tree: EntryTree
  dispose(): Promise<void>
}

interface ReviewedMountConfig {
  baseUrl: string
  spec: ReviewedComposition
  review: CompositionReview
  releaseClosure: () => void
}

const mountedTrees = new WeakMap<ReviewedMountConfig, CompositionMount>()

/** Validate configured rows too: upstream logs a failed create before it can
 * enter tree.store, so iterating only the store would acknowledge no work. */
function validateInventory(group: import('./upstream/loader/src/config/group.ts').EntryGroup): void {
  for (const row of group.data) {
    const entry = group.tree.store[row.id]
    if (!entry || entry.parent !== group || entry.options !== row) {
      throw new Error(`composition row ${JSON.stringify(row.id ?? row.name)} was not created`)
    }
    if (entry.disabled) continue
    if (entry.subgroup) validateInventory(entry.subgroup)
    if (entry.subtree) validateInventory(entry.subtree.root)
  }
}

/** Trusted carrier, following upstream PresetTree's explicit loader inject.
 * The Native wrapper never gains a second loader or a global service grant. */
class ReviewedCarrier {
  static inject = ['loader']
  private readonly tree: ReviewedTree
  private disposed = false

  constructor(private readonly ctx: Context, private readonly config: ReviewedMountConfig) {
    this.tree = new ReviewedTree(ctx, config.baseUrl, config.spec)
    mountedTrees.set(config, { tree: this.tree, dispose: () => this.dispose() })
  }

  private async dispose(): Promise<void> {
    if (this.disposed) return
    this.disposed = true
    const fibers = [...new Set([...this.tree.entries()].map(entry => entry.fiber)
      .filter((fiber): fiber is Fiber => fiber !== undefined))].reverse()
    const pending = this.tree.getTasks()
    let stopError: unknown
    try { this.tree.root.stop() } catch (error) { stopError = error }
    const outcomes = await Promise.allSettled([
      ...pending,
      ...fibers.map(fiber => Promise.resolve().then(() => fiber.dispose())),
    ])
    try {
      await this.tree.await()
      if (stopError !== undefined) throw stopError
      const failed = outcomes.find((outcome): outcome is PromiseRejectedResult => outcome.status === 'rejected')
      if (failed) throw failed.reason
    } finally { this.config.releaseClosure() }
  }

  async *[Service.init]() {
    yield () => this.dispose()
    await this.tree.root.update(this.config.review.entries)
    await this.tree.await()
    validateInventory(this.tree.root)
    for (const entry of this.tree.entries()) {
      if (entry.disabled) continue
      if (!entry.fiber || entry.fiber.uid === null) throw new Error(`composition row ${JSON.stringify(entry.options.id ?? entry.options.name)} failed activation`)
      await entry.fiber.await()
      const missing = Object.keys(entry.fiber.inject).filter(name => entry.fiber!.ctx.get(name) === undefined)
      if (missing.length || entry.fiber.state !== FiberState.ACTIVE) throw new Error(`composition row ${JSON.stringify(entry.options.id ?? entry.options.name)} is not active`)
    }
    const leaked = leakedServices(this.ctx, this.ctx.fiber)
    if (leaked.length) throw new Error(`composition published shared-root services: ${leaked.join(', ')}`)
  }
}

/** Called only from the reviewed generated native entry, under its owner. */
export async function mountReviewedComposition(ctx: Context, baseUrl: string, spec: ReviewedComposition): Promise<CompositionMount> {
  const review = reviewComposition(spec)
  // Anonymous legacy structural groups are explicit Group fibers; preserve the
  // source patch identity comparison and normalize only the mounted projection.
  function groups(rows: EntryOptions[]) { for (const row of rows) { if (row.group===true && row.name===undefined) row.name='cordis:group'; if (row.group===true && Array.isArray(row.config)) groups(row.config) } }
  groups(review.entries)
  const installed = loaders.get(ctx.root)
  if (!installed) throw new Error('composition loader was not installed by this host')
  await installed.fiber.await()
  const loaderCtx = ctx.extend({
    baseUrl,
    [Context.isolate]: Object.assign(Object.create(ctx[Context.isolate]), {
      loader: installed.ctx[Context.isolate].loader,
    }),
  })
  const config: ReviewedMountConfig = { baseUrl, spec, review, releaseClosure: admitReviewedClosure(baseUrl, spec.files) }
  const carrier = loaderCtx.plugin(ReviewedCarrier, config)
  let disposed = false
  const dispose = async () => {
    if (disposed) return
    disposed = true
    try {
      await mountedTrees.get(config)?.dispose()
      await carrier.dispose()
    } finally { config.releaseClosure() }
  }
  ctx.effect(() => dispose, 'reviewed DSH composition teardown')
  try {
    await carrier.await()
    const mount = mountedTrees.get(config)
    if (!mount || carrier.state !== FiberState.ACTIVE) throw new Error('reviewed composition carrier did not activate')
    return { tree: mount.tree, dispose }
  } catch (error) {
    await dispose().catch(() => undefined)
    throw error
  }
}

/** Reused fiber-identity/root-realm audit from DSH agent-presets/src/mount.ts. */
function leakedServices(ctx: Context, mount: Fiber): string[] {
  const rootIsolate = ctx.root[Context.isolate]
  const leaked: string[] = []
  for (const key of Object.getOwnPropertySymbols(ctx.reflect.store)) {
    const impl = ctx.reflect.store[key]
    if (impl === undefined) continue
    let current = impl.fiber
    while (current !== mount && current.parent.fiber !== current) current = current.parent.fiber
    if (current === mount && rootIsolate[impl.name] === key) leaked.push(impl.name)
  }
  return leaked.sort((left, right) => left.localeCompare(right))
}

/** Installer-created Native entry mounts the raw roster row and its selected
 * subtree in one existing owner scope. No process-wide selected preset exists. */
export async function mountReviewedPreset(ctx: Context, baseUrl: string, spec: ReviewedComposition, catalog: PresetCatalog, selected: PreparedPreset): Promise<CompositionMount> {
  if (!selected) throw new Error('agent-presets: explicit selection is required when no default is configured')
  const entries = structuredClone(reviewComposition(spec).entries)
  let found = 0
  function prepare(rows: EntryOptions[]) {
    for (const row of rows) {
      if (row.group === true || row.name === 'cordis:group' || row.name === '@deepseek-ai/cordis-plugin-group') {
        prepare(row.config as EntryOptions[])
      } else if (row.name === AGENT_PRESETS) {
        found++
        row.isolate = { ...row.isolate, agentPresets: true }
        row.config = { codewhale_reviewed: { catalog, selected, base_url: baseUrl } }
      }
    }
  }
  prepare(entries)
  if (found !== 1) throw new Error('exactly one raw roster row is required for a selected preset')
  const source = JSON.stringify([{ insert: entries }])
  const top = await mountReviewedComposition(ctx, baseUrl, { ...spec, layers: [{ path: 'selected-preset.json', sha256: digest(source), source }] })
  try {
    // Service injection requires ACTIVE. Build the raw roster's scope first,
    // then mount its selected subtree, matching upstream's later standing mount.
    const entry = [...top.tree.entries()].find(entry => entry.options.name === AGENT_PRESETS && !entry.disabled)
    const roster = entry?.ctx.get('agentPresets' as any) as { mountSelected(): Promise<unknown> } | undefined
    if (!roster) throw new Error('selected raw roster did not become active')
    await roster.mountSelected()
    return top
  } catch (error) { await top.dispose(); throw error }
}
