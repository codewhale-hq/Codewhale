import { ReviewedLoader, installCompositionLoader } from './dsh/composition.ts'
/**
 * The Cordis root that plugin fibers run under.
 *
 * The host has no turn loop, store, prompt authority or approval. Plugins
 * reach the core only through the shim services here, and every one of them
 * becomes a `registry/*` request that Rust admits or refuses. Service names
 * that belong to the core cannot be provided by a plugin at all.
 */
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { AsyncLocalStorage } from 'node:async_hooks'
import { pathToFileURL } from 'node:url'
import { Context, Inject, Service } from '@deepseek-ai/cordis'
import {
  ErrorCode,
  type ActivateParams,
  type ActivateResult,
  type CommandResultWire,
  type ContentBlockWire,
  type DeactivateResult,
  type HookEvaluateParams,
  type HookVerdictWire,
  type Json,
  type OwnerRef,
  type EntryRef,
  type HarnessRunParams,
  type McpOpenParams,
  type McpRequestParams,
  type McpCloseParams,
  type ToolResultWire,
} from './protocol.ts'
import { RpcError, type RpcPeer } from './rpc.ts'
import { UserQuestionError, PERSONA_PREFIX_SECTION } from './dsh/dsh-message-compat.js'
import { explainImportError } from './dsh/resolve-hooks.ts'
import compatibility from './dsh/compatibility.json' with { type: 'json' }
import { assertSupportedJsonSchema, validateJsonSchemaValue, ToolOutputError } from './dsh/dsh-tools-compat.js'
import { isJson } from './json.ts'
import { makeCoreApi } from './shims/core.ts'
import { gitInvocations, installGitAdapter } from './shims/git.ts'
import { OwnedRegistrations } from './shims/owned.ts'
import { defineShellHooksService,type LocalShellHook } from './shims/shell-hooks.ts'
import { hookExecution, hookVerdict, type LocalHook } from './shims/hooks.ts'
import { PromptSections, definePromptService, type LocalPromptSection } from './shims/prompt.ts'
import { createStorage, type PluginStorage } from './shims/storage.ts'
import {McpDefinitions,defineMcpService,type LocalMcp} from './shims/mcp.ts'
import { AvatarPacks, defineAvatarsService, type LocalAvatarPack } from './shims/avatars.ts'
import { SkillRoots, defineSkillsService, type LocalSkillRoot } from './shims/skills.ts'
import { ownerTier, type HostTier } from './tier.ts'
import {
  commandSpec,
  defineCommandsService,
  makeInvocation,
  type LocalCommand,
  type NormalizedCommand,
} from './shims/commands.ts'

/** Context key carrying the owner record; inherited by every nested fiber. */
export const OWNER = Symbol.for('codewhale.extension-host.owner')

/**
 * Service names a plugin may never provide: each is one authority the Rust
 * core owns (§4.3 of the design). `tools`, `commands` and `logger` are
 * provided by the host root as shims and are refused to plugins for the same
 * reason.
 */
export const REFUSED_SERVICES = new Set(compatibility.refused_services)

/** Services the root provides; `inject` of anything else fails activation. */
const PROVIDED_SERVICES = new Set(compatibility.provided_services)

const ACTIVATE_DEADLINE_MS = 5_000
const DISPOSE_DEADLINE_MS = 2_000

interface HarnessBuiltin { run(params: HarnessRunParams, signal: AbortSignal): Promise<unknown>; dispose(): Promise<void> }

interface McpBuiltin {
  open(params: McpOpenParams, signal: AbortSignal): Promise<unknown>
  request(params: McpRequestParams, signal: AbortSignal): Promise<unknown>
  close(owner: OwnerRef, sessionId: string): Promise<void>
  dispose(): Promise<void>
}

export interface OwnerRecord {
  mcp?: McpBuiltin;
  harness?: HarnessBuiltin;
  ref: OwnerRef
  scope?: EntryRef
  views?: Map<string, OwnerRecord>
  pluginName: string
  fibers: any[]
  /** In-flight `registry/register` requests, awaited before activation acks. */
  pendingRegistrations: Set<Promise<void>>
  refusals: string[]
  tools: Map<number, LocalTool>
  commands: Map<number, LocalCommand<OwnerRecord>>
  shellHooks:Map<number,LocalShellHook<OwnerRecord>>
  hooks: Map<number, LocalHook<OwnerRecord>>
  promptSections: Map<number, LocalPromptSection<OwnerRecord>>
  skillRoots: Map<number, LocalSkillRoot<OwnerRecord>>
  avatarPacks: Map<number, LocalAvatarPack<OwnerRecord>>
  mcpDefinitions:Map<number,LocalMcp<OwnerRecord>>
  storage?: PluginStorage
  warnedAllow?: boolean
  /** Entry modules activated under this owner so far (a plugin may declare several). */
  entries: Set<string>
  /** The plugin's own writable directory, as the core named it at activation. */
  dataDir?: string
  disposing?: Promise<void>
  state: 'activating' | 'active' | 'failed' | 'disposed'
}

interface LocalTool {
  owner: OwnerRecord
  name: string
  handle?: number
  definition: any
  disposed: boolean
}

export const ownerStorage = new AsyncLocalStorage<OwnerRecord>()

function describeError(error: unknown): string {
  if (error instanceof Error) return `${error.name}: ${error.message}`
  return String(error)
}

function withDeadline<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  let timer: NodeJS.Timeout
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${label} exceeded ${ms} ms`)), ms)
    timer.unref()
  })
  return Promise.race([promise, deadline]).finally(() => clearTimeout(timer))
}

interface ToolInvocation {
  owner: OwnerRecord
  session: object | undefined
  agent: object | undefined
  questionTicket: string | undefined
  subagentTicket: string | undefined
  callerTools: readonly string[]
  delegated: boolean
  ticket: string | undefined
  signal: AbortSignal
  active: boolean
  pending: Promise<void>
}

export class HostRoot {
  readonly root: any
  private readonly toolInvocations = new AsyncLocalStorage<ToolInvocation>()
  readonly owners = new Map<string, OwnerRecord>()
  private readonly toolRegistrations: OwnedRegistrations<OwnerRecord, LocalTool>
  private readonly commandRegistrations: OwnedRegistrations<OwnerRecord, LocalCommand<OwnerRecord>>
  private readonly shellRegistrations:OwnedRegistrations<OwnerRecord,LocalShellHook<OwnerRecord>>
  private readonly hookRegistrations: OwnedRegistrations<OwnerRecord, LocalHook<OwnerRecord>>
  private readonly promptSections: PromptSections<OwnerRecord>
  private readonly skillRoots: SkillRoots<OwnerRecord>
  private readonly avatarPacks: AvatarPacks<OwnerRecord>
  private readonly mcpDefinitions:McpDefinitions<OwnerRecord>

  constructor(
    private readonly rpc: RpcPeer,
    /** The trust tier this process serves; it activates only owners of that tier. */
    readonly tier: HostTier,
  ) {
    const root: any = new Context()
    this.root = root
    const host = this
    if (tier === 'plugin') installGitAdapter(() => ownerStorage.getStore() !== undefined)
    this.toolRegistrations = new OwnedRegistrations(
      rpc,
      'tool',
      (owner) => owner.tools,
      (message, owner) => this.log('warn', message, owner),
    )
    this.commandRegistrations = new OwnedRegistrations(
      rpc,
      'command',
      (owner) => owner.commands,
      (message, owner) => this.log('warn', message, owner),
    )
    this.shellRegistrations=new OwnedRegistrations(rpc,'shell_hook',(owner)=>owner.shellHooks,(message,owner)=>this.log('warn',message,owner))
    this.hookRegistrations = new OwnedRegistrations(rpc, 'hook', (owner) => owner.hooks,
      (message, owner) => this.log('warn', message, owner))
    this.promptSections = new PromptSections(rpc, (owner) => owner.promptSections,
      (message, owner) => this.log('warn', message, owner))
    this.mcpDefinitions=new McpDefinitions(rpc,(owner)=>owner.mcpDefinitions,(message,owner)=>this.log('warn',message,owner))
    this.avatarPacks = new AvatarPacks(rpc, owner => owner.avatarPacks, (message, owner) => this.log('warn', message, owner))
    this.skillRoots = new SkillRoots(rpc, (owner) => owner.skillRoots,
      (message, owner) => this.log('warn', message, owner))

    // Cordis already owns listener effects and teardown. Intercept this one
    // event using its supported extension point instead of replacing ctx.on.
    // Actual upstream bridge seams for which this checkpoint has no core
    // projection. Refuse instead of accepting listeners that never fire.
    const unsupportedDshLifecycle = new Set([
      'agent/pre-step', 'agent/turn-stopping',
      'tools/post-execute', 'subagent/start', 'subagent/end',
    ])
    root.on('internal/listener', function (this: any, name: string, callback: any, options: any) {
      if (this[OWNER] && unsupportedDshLifecycle.has(name)) throw new Error(`DSH lifecycle ${name} is not projected by this host checkpoint`)
      if (name === 'agent/created' && options?.global) throw new Error('agent/created listeners are owner-scoped; global is unsupported')
      if (name !== 'tools/pre-execute') return
      const owner: OwnerRecord | undefined = this[OWNER]
      if (!owner) throw new Error('pre-execute listener registered outside an extension owner')
      if (options?.prepend || options?.global) throw new Error('pre-execute listeners use core registration order; prepend/global are unsupported')
      return this.effect(() => host.hookRegistrations.add(
        { owner, name, callback, disposed: false },
        { name, description: 'Programmable tool admission listener' },
      ), 'ctx.on("tools/pre-execute")')
    })

    // Refuse core service names before any plugin can run. The refusal does
    // not depend on who calls: `ctx.root.provide(...)` runs with the root as
    // its context, so an owner check alone could be sidestepped. The one
    // exception is each host shim of its own name, provided once, below.
    const shimClasses = new Map<string, Function>()
    const shimsProvided = new Set<string>()
    const reflect = root.reflect
    const originalProvide = reflect.provide
    reflect.provide = function (this: any, name: string, value: unknown, ...rest: unknown[]) {
      if (REFUSED_SERVICES.has(name)) {
        const shim = shimClasses.get(name)
        const hostShim = shim !== undefined && !shimsProvided.has(name) && value instanceof (shim as any)
        if (!hostShim) {
          throw new Error(`extension may not provide core service \`${name}\`: the Codewhale core owns it`)
        }
        shimsProvided.add(name)
      }
      return originalProvide.call(this, name, value, ...rest)
    }

    // Logger shim: every Cordis log line becomes a `log` notification.
    root.logger.exporter({
      colors: false,
      export: (message: any) => {
        const fiber = message.fiber?.deref?.()
        const owner: OwnerRecord | undefined = fiber?.ctx?.[OWNER]
        const text = (message.args ?? []).map((arg: unknown) => (arg instanceof Error ? describeError(arg) : typeof arg === 'string' ? arg : safeStringify(arg))).join(' ')
        host.log(message.type === 'error' ? 'error' : message.type === 'warn' ? 'warn' : 'info', `[${message.name}] ${text}`, owner)
      },
    })

    class ToolsShim extends Service {
      constructor(ctx: any) {
        super(ctx, 'tools')
      }

      get(name: unknown, agent?: unknown) {
        const invocation = host.toolInvocations.getStore()
        if (typeof name !== 'string' || !invocation || !invocation.active || invocation.agent === undefined || agent !== invocation.agent) return undefined
        return invocation.callerTools.includes(name) ? Object.freeze({ name }) : undefined
      }

      /** DSH `ctx.tools.register(defineTool(...))`: returns an idempotent disposer. */
      register(definition: any, options?: unknown) {
        const ctx: any = this.ctx
        const owner: OwnerRecord | undefined = ctx[OWNER]
        if (!owner) throw new Error('tools.register called outside an extension owner')
        if (options !== undefined && (!options || typeof options !== 'object' || Array.isArray(options) || Reflect.ownKeys(options).length !== 0)) {
          throw new TypeError('tools.register options are unsupported by the Codewhale DSH 0.2.1-rc.2 definition adapter')
        }
        validateDefinition(definition)
        return ctx.effect(() => host.addTool(owner, definition), `tools.register(${JSON.stringify(definition.name)})`)
      }
    }
    // Plugins share one process, so the owner token is not a boundary
    // between them (design §4.4, threat 3). Freezing a shim at least stops
    // the direct route of one plugin rewriting `register` for every other
    // plugin; shared globals remain, and the approval card says so.
    Object.freeze(ToolsShim.prototype)
    const CommandsShim = defineCommandsService<OwnerRecord>({
      ownerOf: (ctx) => ctx[OWNER],
      addCommand: (owner, command) => host.addCommand(owner, command),
    })
    const PromptShim = definePromptService<OwnerRecord>({
      ownerOf: (ctx) => ctx[OWNER],
      promptSections: this.promptSections,
    })
    const ShellHooksShim=defineShellHooksService<OwnerRecord>({ownerOf:(ctx)=>ctx[OWNER],registrations:this.shellRegistrations})
    const McpShim=defineMcpService<OwnerRecord>({ownerOf:(ctx)=>(ctx as Context & { [OWNER]?: OwnerRecord })[OWNER],definitions:this.mcpDefinitions})
    const AvatarsShim = defineAvatarsService<OwnerRecord>({ ownerOf: (ctx) => ctx[OWNER], avatarPacks: this.avatarPacks })
    const SkillsShim = defineSkillsService<OwnerRecord>({ ownerOf: (ctx) => ctx[OWNER], skillRoots: this.skillRoots })
    class StorageShim extends Service {
      constructor(ctx: any) { super(ctx, 'storage') }
      private api(): PluginStorage {
        const owner: OwnerRecord | undefined = (this.ctx as any)[OWNER]
        if (!owner || !owner.dataDir) throw new Error('storage requires an active extension owner and data directory')
        owner.storage ??= createStorage({
          dataDir: owner.dataDir,
          isActive: () => (owner.state === 'activating' || owner.state === 'active') && !owner.disposing,
        })
        return owner.storage
      }
      get(key: string) { return this.api().get(key) }
      set(key: string, value: Json) { return this.api().set(key, value) }
      delete(key: string) { return this.api().delete(key) }
    }
    class SessionsShim extends Service {
      constructor(ctx: any) { super(ctx, 'sessions') }
      async flush(session: unknown): Promise<boolean> {
        const invocation = host.toolInvocations.getStore()
        const owner: OwnerRecord | undefined = (this.ctx as any)[OWNER]
        const live = () => invocation && invocation.active && !invocation.signal.aborted
          && invocation.owner === owner && owner?.state === 'active' && !owner.disposing
          && invocation.ticket !== undefined && session === invocation.session && session !== undefined
        if (!live()) throw new RpcError(ErrorCode.NotAvailable, 'sessions.flush requires this live tool invocation’s session')
        const call = invocation!
        const run = async () => {
          if (!live()) throw new RpcError(ErrorCode.NotAvailable, 'session checkpoint invocation ended')
          const result = await host.rpc.request<unknown>('session/checkpoint', { owner: call.owner.ref, ticket: call.ticket }, call.signal)
          if (!live()) throw new RpcError(ErrorCode.NotAvailable, 'session checkpoint invocation ended')
          if (typeof result !== 'boolean') throw new RpcError(ErrorCode.Internal, 'invalid session checkpoint receipt')
          return result
        }
        const result = call.pending.then(run)
        call.pending = result.then(() => undefined, () => undefined)
        return result
      }
    }
    class UserQuestionsShim extends Service {
      constructor(ctx: any) { super(ctx, 'userQuestions') }
      async ask(request: { agent?: unknown; questions: unknown; signal?: AbortSignal; wait?: unknown }) {
        const invocation = host.toolInvocations.getStore()
        const owner: OwnerRecord | undefined = (this.ctx as any)[OWNER]
        const data = (value: unknown, allowed: string[]): Record<string, unknown> => {
          if (value === null || typeof value !== 'object' || Array.isArray(value)
            || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) throw new RpcError(ErrorCode.InvalidParams, 'expected a plain question object')
          const result: Record<string, unknown> = Object.create(null)
          for (const key of Reflect.ownKeys(value)) {
            const descriptor = Object.getOwnPropertyDescriptor(value, key)!
            if (typeof key !== 'string' || !allowed.includes(key) || !descriptor.enumerable || !('value' in descriptor)) throw new RpcError(ErrorCode.InvalidParams, 'unsupported question property')
            if (descriptor.value !== undefined) result[key] = descriptor.value
          }
          return result
        }
        const list = (value: unknown, maximum: number): unknown[] => {
          if (!Array.isArray(value) || value.length > maximum || Reflect.ownKeys(value).length !== value.length + 1) throw new RpcError(ErrorCode.InvalidParams, 'invalid question array')
          return Array.from({ length: value.length }, (_, index) => {
            const descriptor = Object.getOwnPropertyDescriptor(value, String(index))
            if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) throw new RpcError(ErrorCode.InvalidParams, 'invalid question array item')
            return descriptor.value
          })
        }
        const input = data(request, ['agent', 'questions', 'signal', 'wait'])
        if (input.wait !== undefined) throw new RpcError(ErrorCode.NotAvailable, 'timed or continued questions are not yet supported')
        request = input as typeof request
        if (request.signal !== undefined && !(request.signal instanceof AbortSignal)) {
          throw new RpcError(ErrorCode.InvalidParams, 'question signal must be an AbortSignal')
        }
        if (request.signal?.aborted) throw new UserQuestionError('ask_user_question was aborted before the user answered', 'ASK_ABORTED')
        const items = list(request.questions, 10)
        if (items.length === 0) throw new UserQuestionError('ask_user_question requires at least one question', 'EMPTY_QUESTIONS')
        if (request.agent === undefined) throw new RpcError(ErrorCode.NotAvailable, 'userQuestions.ask requires this live tool invocation’s agent')
        if (!invocation || !invocation.active || invocation.signal.aborted || invocation.agent === undefined || request.agent !== invocation.agent) {
          throw new UserQuestionError('human interaction requires the exact live calling agent when an agent is supplied', 'CALLER_NOT_LIVE')
        }
        if (invocation.owner !== owner || owner?.state !== 'active' || owner.disposing) throw new RpcError(ErrorCode.NotAvailable, 'userQuestions.ask service owner is no longer current')
        if (invocation.delegated) throw new UserQuestionError("human interaction is unavailable while the calling agent is owned by another live agent; include the unresolved question or decision in the child agent's final result", 'DELEGATED_CALLER')
        const live = () => invocation.active && !invocation.signal.aborted
          && invocation.owner === owner && owner?.state === 'active' && !owner.disposing
          && invocation.questionTicket !== undefined && request.agent === invocation.agent
        if (!live()) throw new RpcError(ErrorCode.NotAvailable, 'userQuestions.ask requires this live tool invocation’s question ticket')
        const signal = request.signal === undefined ? invocation.signal : AbortSignal.any([invocation.signal, request.signal])
        const questions = items.map(value => {
          const question = data(value, ['id', 'question', 'header', 'detail', 'options', 'multiSelect', 'intent'])
          const options = list(question.options === undefined ? [] : question.options, 1000)
          const intent = question.intent === undefined ? undefined : data(question.intent, ['kind', 'approve', 'callId'])
          if (intent !== undefined && intent.kind !== 'plan-review') throw new RpcError(ErrorCode.InvalidParams, 'unsupported question intent')
          return {
            id: question.id, question: question.question, header: question.header === undefined ? '' : question.header,
            ...(question.detail === undefined ? {} : { detail: question.detail }),
            ...(intent === undefined ? {} : { intent: { kind: 'plan-review', approve: intent.approve,
              ...(intent.callId === undefined ? {} : { call_id: intent.callId }) } }),
            options: options.map(option => {
              const item = data(option, ['label', 'description'])
              return { label: item.label, description: item.description === undefined ? '' : item.description }
            }),
            allow_free_text: true, multi_select: question.multiSelect === undefined ? false : question.multiSelect,
          }
        })
        if (!isJson(questions)) throw new RpcError(ErrorCode.InvalidParams, 'question data must be lossless JSON')
        for (const question of questions) {
          if (question.intent === undefined) continue
          if (!question.options.some(option => option.label === question.intent!.approve)) throw new UserQuestionError('question ' + question.id + ' declares an intent whose approve label names none of its options', 'BAD_INTENT')
          if (question.detail === undefined) throw new UserQuestionError('question ' + question.id + ' declares an intent without the detail it reviews', 'BAD_INTENT')
        }
        if (Buffer.byteLength(JSON.stringify(questions)) > 1024 * 1024) throw new RpcError(ErrorCode.InvalidParams, 'question payload exceeds 1 MiB')
        const call = invocation!
        const run = async () => {
          if (!live() || signal.aborted) throw new RpcError(ErrorCode.Cancelled, 'question invocation ended')
          const result = await host.rpc.request<unknown>('user-questions/ask', { owner: call.owner.ref, ticket: call.questionTicket, questions }, signal).catch(error => {
            if (error instanceof RpcError && isJson(error.data)
              && error.data !== null && typeof error.data === 'object' && !Array.isArray(error.data)) {
              const code = error.data.user_question_error
              if ((error.code === ErrorCode.NotAvailable && code === 'NO_PROVIDER')
                || (error.code === ErrorCode.Cancelled && code === 'ASK_CANCELLED')) {
                throw new UserQuestionError(error.message, code, { cause: error })
              }
            }
            if (signal.aborted) throw new UserQuestionError('ask_user_question was aborted before the user answered', 'ASK_ABORTED', { cause: error })
            throw error
          })
          if (!live() || signal.aborted) throw new RpcError(ErrorCode.Cancelled, 'question invocation ended')
          if (!isJson(result) || result === null || typeof result !== 'object' || Array.isArray(result) || !Array.isArray(result.answers)) {
            throw new RpcError(ErrorCode.Internal, 'invalid question answer receipt')
          }
          const answers = questions.map(question => ({ id: question.id, selected: [] as string[], custom: undefined as string | undefined, skipped: false, rows: 0 }))
          for (const row of result.answers) {
            if (row === null || typeof row !== 'object' || Array.isArray(row) || typeof row.label !== 'string' || typeof row.value !== 'string') throw new RpcError(ErrorCode.Internal, 'invalid question answer row')
            const answer = answers.find(answer => answer.id === row.id)
            const question = questions.find(question => question.id === row.id)
            if (!answer || !question || answer.skipped) throw new RpcError(ErrorCode.Internal, 'question answer identity mismatch')
            if (row.kind === 'choice' && row.label === row.value && question.options.some(option => option.label === row.value) && !answer.selected.includes(row.value)) answer.selected.push(row.value)
            else if (row.kind === 'custom' && row.label === '' && row.value.trim() !== '' && answer.custom === undefined) answer.custom = row.value
            else if (row.kind === 'skipped' && row.label === '' && row.value === '' && answer.rows === 0) answer.skipped = true
            else throw new RpcError(ErrorCode.Internal, 'ambiguous question answer receipt')
            answer.rows += 1
            if (!question.multi_select && answer.selected.length + (answer.custom === undefined ? 0 : 1) > 1) throw new RpcError(ErrorCode.Internal, 'invalid single-select answer')
          }
          if (answers.some(answer => answer.rows === 0)) throw new RpcError(ErrorCode.Internal, 'missing question answer')
          return { answers: answers.map(({ id, selected, custom }) => ({ id, selected, ...(custom === undefined ? {} : { custom }) })) }
        }
        const result = call.pending.then(run)
        call.pending = result.then(() => undefined, () => undefined)
        let cancel: () => void = () => {}
        const cancelled = new Promise<never>((_, reject) => {
          cancel = () => reject(new UserQuestionError('ask_user_question was aborted before the user answered', 'ASK_ABORTED'))
          signal.addEventListener('abort', cancel, { once: true })
          if (signal.aborted) cancel()
        })
        return Promise.race([result, cancelled]).finally(() => signal.removeEventListener('abort', cancel))
      }
    }
    const SPAWN_PROVIDER = Object.freeze({
      name: 'spawn',
      inheritsParentContext: false,
      capabilities: Object.freeze({ persona: true, toolFilter: true, agentOptions: true }),
    })
    const PERSONA_ORDER = 0
    class SubagentsShim extends Service {
      constructor(ctx: any) { super(ctx, 'subagents') }
      getProvider(name: unknown) { return name === 'spawn' ? SPAWN_PROVIDER : undefined }
      async startActivation(request: any) {
        const invocation = host.toolInvocations.getStore()
        const owner: OwnerRecord | undefined = (this.ctx as any)[OWNER]
        const plain = (value: unknown, allowed: string[], what: string): Record<string, unknown> => {
          if (value === null || typeof value !== 'object' || Array.isArray(value)
            || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) throw new RpcError(ErrorCode.InvalidParams, `expected a plain ${what} object`)
          const result: Record<string, unknown> = Object.create(null)
          for (const key of Reflect.ownKeys(value)) {
            const descriptor = Object.getOwnPropertyDescriptor(value, key)!
            if (typeof key !== 'string' || !allowed.includes(key) || !('value' in descriptor)) throw new RpcError(ErrorCode.NotAvailable, `unsupported ${what} property ${String(key)}`)
            if (descriptor.value !== undefined) result[key] = descriptor.value
          }
          return result
        }
        const input = plain(request, ['provider', 'label', 'childId', 'request', 'signal', 'delivery'], 'activation')
        if (input.provider !== 'spawn') throw new RpcError(ErrorCode.NotAvailable, 'only the spawn provider is available')
        const spawn = plain(input.request, ['parent', 'prompt', 'agentOptions'], 'spawn request')
        const options = spawn.agentOptions === undefined ? {} : plain(spawn.agentOptions, ['model'], 'agentOptions')
        if (options.model !== undefined && typeof options.model !== 'string') throw new RpcError(ErrorCode.InvalidParams, 'agentOptions.model must be a string')
        const delivery = input.delivery === undefined ? 'parent' : input.delivery
        if (delivery !== 'caller' && delivery !== 'parent') throw new RpcError(ErrorCode.InvalidParams, 'delivery must be caller or parent')
        const childId = input.childId
        if (typeof childId !== 'string' || !/^[A-Za-z0-9_.:-]{1,128}$/.test(childId)) throw new RpcError(ErrorCode.InvalidParams, 'childId must be 1-128 letters, digits, or _.:-')
        if (!Array.isArray(spawn.prompt) || spawn.prompt.length === 0) throw new RpcError(ErrorCode.InvalidParams, 'prompt must be a non-empty content array')
        const prompt = spawn.prompt.map((block: unknown) => {
          const item = plain(block, ['type', 'text'], 'prompt block')
          if (item.type !== 'text' || typeof item.text !== 'string') throw new RpcError(ErrorCode.NotAvailable, 'only text prompt blocks are supported')
          return item.text
        }).join('')
        if (input.signal !== undefined && !(input.signal instanceof AbortSignal)) throw new RpcError(ErrorCode.InvalidParams, 'signal must be an AbortSignal')
        if (!invocation || !invocation.active || invocation.signal.aborted || invocation.agent === undefined || spawn.parent !== invocation.agent) {
          throw new RpcError(ErrorCode.NotAvailable, 'startActivation requires the exact live calling agent as parent')
        }
        if (invocation.owner !== owner || owner?.state !== 'active' || owner.disposing) throw new RpcError(ErrorCode.NotAvailable, 'subagents service owner is no longer current')
        if (invocation.delegated || invocation.subagentTicket === undefined) throw new RpcError(ErrorCode.NotAvailable, 'child agents are only available to a root tool invocation')
        const signal = input.signal === undefined ? invocation.signal : AbortSignal.any([invocation.signal, input.signal as AbortSignal])
        signal.throwIfAborted()
        let persona: string | undefined
        const denied = new Set<string>()
        let open = true
        const sealed = () => { if (!open) throw new RpcError(ErrorCode.NotAvailable, 'the child creation window has closed') }
        const child = Object.freeze({
          id: childId,
          ctx: Object.freeze({
            systemPrompt: Object.freeze({
              getSectionOrder(name: unknown) {
                sealed()
                if (name !== 'DEPLOYMENT_PERSONA_PREFIX') throw new RpcError(ErrorCode.NotAvailable, 'only the deployment persona slot is available')
                return PERSONA_ORDER
              },
              section(section: unknown) {
                sealed()
                const value = plain(section, ['name', 'order', 'text'], 'system prompt section')
                if (value.name !== PERSONA_PREFIX_SECTION || value.order !== PERSONA_ORDER || typeof value.text !== 'string') {
                  throw new RpcError(ErrorCode.NotAvailable, 'only the child deployment persona section is supported')
                }
                if (persona !== undefined) throw new RpcError(ErrorCode.InvalidParams, 'the child persona is already set')
                if (Buffer.byteLength(value.text) > 32 * 1024) throw new RpcError(ErrorCode.InvalidParams, 'child persona exceeds 32768 bytes')
                persona = value.text
                return () => undefined
              },
            }),
            tools: Object.freeze({
              restrict(filter: unknown) {
                sealed()
                const value = plain(filter, ['deny'], 'tool filter')
                if (!Array.isArray(value.deny) || value.deny.some(name => typeof name !== 'string' || name === '')) throw new RpcError(ErrorCode.InvalidParams, 'deny must be a list of tool names')
                for (const name of value.deny as string[]) denied.add(name)
                return () => undefined
              },
            }),
          }),
        })
        const listeners = ((host.root as any).events?._hooks?.['agent/created'] ?? [])
          .filter((hook: any) => !hook.global && hook.ctx?.[OWNER] === owner)
          .map((hook: any) => hook.callback)
        try {
          await Promise.all(listeners.map(async (callback: Function) => callback({ agent: child })))
        } finally {
          open = false
        }
        signal.throwIfAborted()
        const call = invocation
        const rpc = (method: string, params: Record<string, unknown>, abort: AbortSignal) =>
          host.rpc.request<any>(method, { owner: call.owner.ref, ticket: call.subagentTicket, child_id: childId, ...params }, abort)
        const started = await rpc('subagent/start', {
          prompt,
          ...(options.model === undefined ? {} : { model: options.model }),
          ...(persona === undefined ? {} : { persona }),
          denied_tools: [...denied],
          delivery,
        }, signal)
        if (started === null || typeof started !== 'object' || started.child_id !== childId || typeof started.message_id !== 'string') {
          throw new RpcError(ErrorCode.Internal, 'invalid child activation receipt')
        }
        let result: Promise<unknown> | undefined
        let disposed: Promise<void> | undefined
        return Object.freeze({
          childId,
          messageId: started.message_id,
          get result() {
            result ??= rpc('subagent/result', {}, call.signal).then((value: any) => {
              if (value === null || typeof value !== 'object' || typeof value.stop_reason !== 'string' || !Array.isArray(value.output)) {
                throw new RpcError(ErrorCode.Internal, 'invalid child result receipt')
              }
              return Object.freeze({
                stopReason: value.stop_reason,
                output: Object.freeze(value.output.map((block: any) => Object.freeze({ type: 'text', text: String(block.text) }))),
                ...(value.omitted_blocks > 0 ? { diagnostic: `${value.omitted_blocks} non-text output block(s) are not supported` } : {}),
              })
            })
            return result
          },
          dispose() {
            disposed ??= rpc('subagent/dispose', {}, call.signal).then(() => undefined)
            return disposed
          },
        })
      }
    }
    class AgentsShim extends Service {
      constructor(ctx: any) { super(ctx, 'agents') }
    }
    class SystemPromptShim extends Service {
      constructor(ctx: any) { super(ctx, 'systemPrompt') }
      section() { throw new RpcError(ErrorCode.NotAvailable, "system prompt sections are only available on a child agent's agent/created context") }
      getSectionOrder() { throw new RpcError(ErrorCode.NotAvailable, "system prompt sections are only available on a child agent's agent/created context") }
    }
    Object.freeze(SubagentsShim.prototype)
    Object.freeze(AgentsShim.prototype)
    Object.freeze(SystemPromptShim.prototype)
    Object.freeze(UserQuestionsShim.prototype)
    Object.freeze(SessionsShim.prototype)
    Object.freeze(StorageShim.prototype)
    shimClasses.set('tools', ToolsShim)
    shimClasses.set('commands', CommandsShim)
    shimClasses.set('prompt', PromptShim)
    shimClasses.set('storage', StorageShim)
    shimClasses.set('sessions', SessionsShim)
    shimClasses.set('userQuestions', UserQuestionsShim)
    shimClasses.set('subagents', SubagentsShim)
    shimClasses.set('agents', AgentsShim)
    shimClasses.set('systemPrompt', SystemPromptShim)
    shimClasses.set('skills', SkillsShim)
    shimClasses.set('avatars', AvatarsShim)
    shimClasses.set('mcp',McpShim)
    shimClasses.set('shellHooks',ShellHooksShim)
    root.plugin(ToolsShim)
    root.plugin(CommandsShim)
    root.plugin(PromptShim)
    root.plugin(StorageShim)
    root.plugin(SessionsShim)
    root.plugin(UserQuestionsShim)
    root.plugin(SubagentsShim)
    root.plugin(AgentsShim)
    root.plugin(SystemPromptShim)
    root.plugin(SkillsShim)
    root.plugin(AvatarsShim)
    root.plugin(McpShim)
    root.plugin(ShellHooksShim)
    shimClasses.set('loader', ReviewedLoader)
    installCompositionLoader(root)
  }

  log(level: string, msg: string, owner?: OwnerRecord) {
    const params: Record<string, string> = { level, msg: msg.slice(0, 8192) }
    if (owner) params.plugin_id = owner.ref.plugin_id
    this.rpc.notify('log', params)
  }

  /** Called inside the owner's effect; returns the effect's cleanup. */
  private addTool(owner: OwnerRecord, definition: any): () => void {
    const local: LocalTool = { owner, name: definition.name, definition, disposed: false }
    return this.toolRegistrations.add(local, {
      name: definition.name,
      description: String(definition.description ?? ''),
      input_schema: definition.parameters ?? { type: 'object', properties: {} },
    })
  }

  private addCommand(owner: OwnerRecord, definition: NormalizedCommand): () => void {
    const local: LocalCommand<OwnerRecord> = { owner, name: definition.name, definition, disposed: false }
    return this.commandRegistrations.add(local, commandSpec(definition))
  }

  /**
   * `ext/activate`: load one `native` entry of a plugin under its owner. A
   * manifest may declare several entries; the core sends one `ext/activate`
   * per entry, in order, under the same owner token, and every entry becomes a
   * fiber of that one owner. A further entry is accepted only after the
   * previous one finished activating, for the same plugin, and only once per
   * path. Core-selected Native scopes have independent fibers; failure withdraws
   * that entry while siblings remain active. An unscoped owner stays atomic.
   */
  async activate(params: ActivateParams): Promise<ActivateResult> {
    // This process serves one tier. An owner of the other tier is refused
    // outright, before anything of it is read or loaded.
    const wanted = ownerTier(params.owner.plugin_id)
    if (wanted !== this.tier) {
      throw new RpcError(
        ErrorCode.InvalidParams,
        `owner ${JSON.stringify(params.owner.plugin_id)} belongs to the ${wanted} tier, but this host serves the ${this.tier} tier`,
      )
    }
    const key = params.owner.owner_token
    let parent=this.owners.get(key)
    const scopeKey=params.scope===undefined ? undefined : `${params.scope.path}\0${params.scope.sha256}`
    if (scopeKey!==undefined) {
      if (params.scope?.path!==params.entry.path || params.scope?.sha256!==params.entry.sha256) return {status:'failed',diagnostic:'scope does not match the core-selected entry'}
      parent ??= {ref:params.owner,pluginName:params.plugin_name,fibers:[],pendingRegistrations:new Set(),refusals:[],tools:new Map(),commands:new Map(),hooks:new Map(),shellHooks:new Map(),promptSections:new Map(),skillRoots:new Map(),avatarPacks:new Map(),mcpDefinitions:new Map(),entries:new Set(),views:new Map(),state:'active',...(params.data_dir===undefined?{}:{dataDir:params.data_dir})}
      if (parent.state!=='active' || parent.ref.plugin_id!==params.owner.plugin_id || parent.ref.generation!==params.owner.generation || parent.pluginName!==params.plugin_name) return {status:'failed',diagnostic:'scope owner was withdrawn'}
      parent.views ??=new Map()
      this.owners.set(key,parent)
    }
    const existing = scopeKey===undefined ? this.owners.get(key) : parent!.views!.get(scopeKey)
    if (existing) {
      if (existing.state !== 'active') return { status: 'failed', diagnostic: 'owner token already active' }
      if (existing.ref.plugin_id !== params.owner.plugin_id || existing.pluginName !== params.plugin_name) {
        return { status: 'failed', diagnostic: 'owner token already active for another plugin' }
      }
      if (existing.entries.has(params.entry.path)) {
        return { status: 'failed', diagnostic: `entry ${params.entry.path} is already activated under this owner` }
      }
      existing.state = 'activating'
    }
    const owner: OwnerRecord = existing ?? {
      ref: params.owner,
      ...(params.scope===undefined ? {} : {scope:params.scope}),
      pluginName: params.plugin_name,
      fibers: [],
      pendingRegistrations: new Set(),
      refusals: [],
      tools: new Map(),
      commands: new Map(),
      hooks: new Map(),
      shellHooks:new Map(),
      promptSections: new Map(),
      skillRoots: new Map(),
      avatarPacks: new Map(),
      mcpDefinitions:new Map(),
      entries: new Set(),
      ...(params.data_dir === undefined ? {} : { dataDir: params.data_dir }),
      state: 'activating',
    }
    if (!existing) {if(scopeKey===undefined)this.owners.set(key,owner);else parent!.views!.set(scopeKey,owner)}
    owner.entries.add(params.entry.path)
    try {
      const bytes = await readFile(params.entry.path)
      const digest = createHash('sha256').update(bytes).digest('hex')
      if (digest !== params.entry.sha256) {
        throw new Error(`entry ${params.entry.path} changed after review (sha256 ${digest.slice(0, 12)}…)`)
      }
      const module = await ownerStorage.run(owner, () => import(pathToFileURL(params.entry.path).href)).catch((error) => {
        throw explainImportError(error)
      })
      if (this.tier === 'builtin' && owner.ref.plugin_id === 'host:harness') {
        if (existing || typeof module.createHarnessModule !== 'function') throw new Error('invalid harness builtin module')
        owner.harness = module.createHarnessModule(this.rpc, Object.freeze(structuredClone(owner.ref))) as HarnessBuiltin
        owner.state = 'active'
        return { status: 'ok', tools: [], commands: [] }
      }
      if (this.tier === 'builtin' && owner.ref.plugin_id === 'host:mcp') {
        if (existing || typeof module.createMcpModule !== 'function') throw new Error('invalid MCP builtin module')
        owner.mcp = module.createMcpModule(this.rpc, Object.freeze(structuredClone(owner.ref))) as McpBuiltin
        owner.state = 'active'
        return { status: 'ok', tools: [], commands: [] }
      }
      const plugin = pickPlugin(module)
      const missing = Object.keys(Inject.resolve(plugin.inject)).filter((name) => !PROVIDED_SERVICES.has(name))
      if (missing.length > 0) {
        throw new Error(`requires ${missing.map((name) => `\`${name}\``).join(', ')}, not provided by the Codewhale extension host in this phase`)
      }
      const ownerCtx = this.root.extend({ [OWNER]: owner })
      const fiber = ownerStorage.run(owner, () => ownerCtx.plugin(plugin, params.config ?? {}))
      owner.fibers.push(fiber)
      await withDeadline(Promise.resolve(fiber.await()), ACTIVATE_DEADLINE_MS, 'activation')
      const pending = pendingFibers(fiber)
      if (pending.length > 0) {
        throw new Error(`plugin fibers are waiting on services the host does not provide: ${pending.join(', ')}`)
      }
      while (owner.pendingRegistrations.size > 0) {
        await Promise.allSettled([...owner.pendingRegistrations])
      }
      if (owner.refusals.length > 0) throw new Error(owner.refusals.join('; '))
      if (owner.state !== 'activating' || owner.disposing || (scopeKey !== undefined && parent?.state !== 'active')) throw new Error('entry was withdrawn while activating')
      owner.state = 'active'
      return {
        status: 'ok',
        tools: [...owner.tools.values()].map((tool) => tool.name).sort(),
        commands: [...owner.commands.values()].map((command) => command.name).sort(),
      }
    } catch (error) {
      owner.state = 'failed'
      // Return the activation cause promptly, while retaining a dirty owner
      // for Rust's bounded ext/deactivate receipt. Never hide an unfinished
      // disposer by deleting the record before Core can observe it.
      const cleanup = this.disposeOwner(owner)
      void cleanup.then(() => {
        this.toolRegistrations.forget(owner)
        this.commandRegistrations.forget(owner)
        this.hookRegistrations.forget(owner);this.shellRegistrations.forget(owner)
        this.promptSections.forget(owner)
        this.skillRoots.forget(owner);this.avatarPacks.forget(owner);this.mcpDefinitions.forget(owner)
        if (scopeKey === undefined) {
          if (this.owners.get(key) === owner) this.owners.delete(key)
        } else if (parent?.views?.get(scopeKey) === owner) parent.views.delete(scopeKey)
      }, () => undefined)
      return { status: 'failed', diagnostic: describeError(error) }
    }
  }

  async harnessRun(params: HarnessRunParams, signal: AbortSignal): Promise<unknown> {
    const owner = this.owners.get(params.owner.owner_token)
    if (this.tier !== 'builtin' || params.owner.plugin_id !== 'host:harness' || owner?.state !== 'active' || owner.ref.generation !== params.owner.generation || !owner.harness || owner.disposing) throw new RpcError(ErrorCode.NotAvailable, 'harness builtin owner is no longer live')
    return owner.harness.run(params, signal)
  }

  private mcpOwner(ref: OwnerRef): McpBuiltin {
    const owner = this.owners.get(ref.owner_token)
    if (this.tier !== 'builtin' || ref.plugin_id !== 'host:mcp' || owner?.state !== 'active' || owner.ref.generation !== ref.generation || !owner.mcp || owner.disposing) {
      throw new RpcError(ErrorCode.NotAvailable, 'MCP builtin owner is no longer live')
    }
    return owner.mcp
  }
  async mcpOpen(params: McpOpenParams, signal: AbortSignal): Promise<unknown> { return this.mcpOwner(params.owner).open(params, signal) }
  async mcpRequest(params: McpRequestParams, signal: AbortSignal): Promise<unknown> { return this.mcpOwner(params.owner).request(params, signal) }
  async mcpClose(params: McpCloseParams): Promise<unknown> { await this.mcpOwner(params.owner).close(params.owner, params.session_id); return {} }

  /** Dispose one owner's fibers (reverse order, async disposers awaited). Memoised. */
  disposeOwner(owner: OwnerRecord): Promise<void> {
    owner.state = 'disposed'
    owner.disposing ??= (async () => {
      for (const view of owner.views?.values() ?? []) await this.disposeOwner(view)
      await owner.mcp?.dispose()
      await owner.harness?.dispose()
      for (const fiber of [...owner.fibers].reverse()) {
        await fiber.dispose()
      }
      // Admission may have answered after a failed fiber's disposer ran.
      // OwnedRegistrations compensates that admission and tracks its cleanup;
      // acknowledge withdrawal only once both phases have settled.
      while (owner.pendingRegistrations.size > 0) {
        await Promise.allSettled([...owner.pendingRegistrations])
      }
      owner.state = 'disposed'
    })()
    return owner.disposing
  }

  async deactivate(ref: OwnerRef, entry?: EntryRef): Promise<DeactivateResult> {
    const parent=this.owners.get(ref.owner_token)
    if (parent && (parent.ref.plugin_id!==ref.plugin_id || parent.ref.generation!==ref.generation)) throw new Error('deactivation owner differs')
    if (entry===undefined && parent?.views) {
      parent.state = 'disposed'
      const results=await Promise.all([...parent.views.values()].map(view=>this.deactivate(ref,view.scope)))
      parent.views.clear()
      this.owners.delete(ref.owner_token)
      return {disposed:results.every(result=>result.disposed),leaked:results.flatMap(result=>result.leaked)}
    }
    const scopeKey=entry===undefined?undefined:`${entry.path}\0${entry.sha256}`
    const owner = scopeKey===undefined?parent:parent?.views?.get(scopeKey)
    if (!owner) return { disposed: true, leaked: [] }
    let disposed = true
    try {
      await withDeadline(this.disposeOwner(owner), DISPOSE_DEADLINE_MS, 'dispose')
    } catch {
      disposed = false
    }
    const leaked = [
      ...[...owner.tools.values()].map((tool) => `tool:${tool.name}`),
      ...[...owner.commands.values()].map((command) => `command:${command.name}`),
      ...[...owner.shellHooks.values()].map((hook)=>`shell_hook:${hook.name}`),
      ...[...owner.hooks.values()].map((hook) => `hook:${hook.name}`),
      ...[...owner.promptSections.values()].map((section) => `prompt_section:${section.name}`),
      ...[...owner.mcpDefinitions.values()].map(server=>`mcp_server:${server.name}`),
      ...[...owner.avatarPacks.values()].map((pack) => `avatar_pack:${pack.name}`),
      ...[...owner.skillRoots.values()].map((root) => `skill_root:${root.name}`),
    ]
    for (const fiber of owner.fibers) {
      for (const effect of fiber.getEffects?.() ?? []) leaked.push(`effect:${effect.label}`)
    }
    this.toolRegistrations.forget(owner)
    this.commandRegistrations.forget(owner)
    this.hookRegistrations.forget(owner);this.shellRegistrations.forget(owner)
    this.promptSections.forget(owner)
    this.skillRoots.forget(owner);this.avatarPacks.forget(owner);this.mcpDefinitions.forget(owner)
    if(scopeKey===undefined)this.owners.delete(ref.owner_token);else parent?.views?.delete(scopeKey)
    return { disposed, leaked }
  }

  async deactivateAll(deadlineMs: number) {
    await withDeadline(
      Promise.allSettled([...this.owners.values()].map((owner) => this.deactivate(owner.ref))),
      deadlineMs,
      'shutdown',
    ).catch(() => undefined)
  }

  async callTool(
    handle: number,
    input: unknown,
    callId: string,
    signal: AbortSignal,
    workspace?: string,
    /** The core's invocation ticket: present only when this call runs under the turn loop's permission gate. */
    ticket?: string,
    identity?: InvocationIdentityWire,
  ): Promise<ToolResultWire> {
    const local = this.toolRegistrations.byHandle.get(handle)
    if (!local || local.disposed || local.owner.state !== 'active') {
      throw new RpcError(ErrorCode.NotAvailable, `tool handle ${handle} is not live`)
    }
    const definition = local.definition
    // `workspace` is the calling session's workspace, per call; `dataDir` is
    // this plugin's own directory. Both are read-only strings.
    // `core` exists only when the core gave this call a ticket (`exec.core` in shims/core.ts).
    const core = ticket === undefined ? {} : { core: makeCoreApi(this.rpc, local.owner.ref, ticket, signal) }
    const agent = workspace && identity?.session_id ? Object.freeze({
      id: identity.agent_id ?? identity.session_id,
      session: Object.freeze({ header: Object.freeze({ cwd: workspace }) }),
    }) : undefined
    const exec = Object.freeze({ signal, callId, args: input, ...callContext(local.owner, workspace, identity), ...core, ...(agent === undefined ? {} : { agent }) })
    const closed = new AbortController()
    const invocation: ToolInvocation = {
      owner: local.owner,
      session: agent?.session,
      agent,
      questionTicket: identity?.question_ticket,
      subagentTicket: identity?.subagent_ticket,
      callerTools: Object.freeze([...(identity?.caller_tools ?? [])]),
      delegated: identity?.agent_id !== undefined,
      ticket: identity?.checkpoint_ticket,
      signal: AbortSignal.any([signal, closed.signal]),
      active: true,
      pending: Promise.resolve(),
    }
    const gitInvocation = workspace && ticket ? {
      workspace,
      core: makeCoreApi(this.rpc, local.owner.ref, ticket, invocation.signal),
      signal: invocation.signal,
      live: () => invocation.active && local.owner.state === 'active' && !local.owner.disposing,
    } : undefined
    const run = ownerStorage.run(local.owner, () => this.toolInvocations.run(invocation, () => gitInvocations.run(gitInvocation, async () => {
      const value = await definition.execute(input, exec)
      return renderResult(definition, input, value)
    })))
    try {
      return await Promise.race([run, abortedBy(signal)])
    } finally {
      invocation.active = false
      closed.abort()
    }
  }

  /** `command/run`: only for a handle the core admitted, and only on a user's own invocation. */
  async callCommand(handle: number, rawInput: string, commandId: string, signal: AbortSignal, workspace?: string, identity?: InvocationIdentityWire): Promise<CommandResultWire> {
    const local = this.commandRegistrations.byHandle.get(handle)
    if (!local || local.disposed || local.owner.state !== 'active') {
      throw new RpcError(ErrorCode.NotAvailable, `command handle ${handle} is not live`)
    }
    const { definition } = local
    const call = makeInvocation(definition.name, rawInput, commandId, signal, callContext(local.owner, workspace, identity))
    const run = ownerStorage.run(local.owner, async () => {
      try {
        const value = await definition.handler(call.invocation)
        return call.complete(value)
      } finally {
        call.close()
      }
    })
    try {
      return await Promise.race([run, abortedBy(signal)])
    } finally {
      call.close()
    }
  }

  async evaluateHook(params: HookEvaluateParams, signal: AbortSignal): Promise<HookVerdictWire> {
    const local = this.hookRegistrations.byHandle.get(params.handle)
    if (!local || local.disposed || local.owner.state !== 'active' || params.event !== local.name) {
      throw new RpcError(ErrorCode.NotAvailable, 'hook handle is not live for this event')
    }
    const exec = hookExecution(params.payload, signal)
    const run = ownerStorage.run(local.owner, async () => hookVerdict(
      await local.callback(exec, async () => ({ kind: 'abstain' })),
      () => {
        if (local.owner.warnedAllow) return
        local.owner.warnedAllow = true
        this.log('warn', 'pre-execute allow is an abstention; Rust still evaluates all admission gates', local.owner)
      },
    ))
    return Promise.race([run, abortedBy(signal)])
  }
}

/** The read-only context a call carries beyond its input; a field the core did not send is absent. */
interface InvocationIdentityWire {
  question_ticket?: string
  subagent_ticket?: string
  caller_tools?: string[]
  checkpoint_ticket?: string
  session_id?: string
  agent_id?: string
  origin_turn_id?: string
}

function callContext(owner: OwnerRecord, workspace: string | undefined, identity?: InvocationIdentityWire) {
  return {
    ...(workspace === undefined ? {} : { workspace }),
    ...(owner.dataDir === undefined ? {} : { dataDir: owner.dataDir }),
    ...(identity?.session_id === undefined ? {} : { sessionId: identity.session_id }),
    ...(identity?.agent_id === undefined ? {} : { agentId: identity.agent_id }),
    ...(identity?.origin_turn_id === undefined ? {} : { originTurnId: identity.origin_turn_id }),
  }
}

/** Rejects as cancelled when `signal` aborts. */
function abortedBy(signal: AbortSignal): Promise<never> {
  return new Promise<never>((_, reject) => {
    const onAbort = () => reject(new RpcError(ErrorCode.Cancelled, 'cancelled'))
    if (signal.aborted) onAbort()
    else signal.addEventListener('abort', onAbort, { once: true })
  })
}

function renderResult(definition: any, input: unknown, value: unknown): ToolResultWire {
  let blocks: unknown = undefined
  if (definition.output !== undefined) {
    if (!isJson(value)) throw new ToolOutputError(definition.name, ['value must be lossless JSON'])
    const violations = validateJsonSchemaValue(definition.output.schema, value)
    if (violations.length > 0) throw new ToolOutputError(definition.name, violations)
    blocks = definition.output.render(input, value)
    if (!Array.isArray(blocks)) throw new ToolOutputError(definition.name, ['output.render must return an array of content blocks'])
  }
  const content: ContentBlockWire[] = []
  if (Array.isArray(blocks)) {
    for (const block of blocks) {
      if (!block || typeof block !== 'object' || (block as any).type !== 'text' || typeof (block as any).text !== 'string') {
        throw new ToolOutputError(definition.name, ['output.render returned an unsupported content block; this host accepts text blocks only'])
      }
      if (Reflect.ownKeys(block).some(key => key !== 'type' && key !== 'text')) throw new ToolOutputError(definition.name, ['output.render returned unsupported text-block fields'])
      content.push({ type: 'text', text: (block as any).text })
    }
  } else {
    content.push({ type: 'text', text: typeof value === 'string' ? value : safeStringify(value) })
  }
  const result: ToolResultWire = { content, is_error: false }
  if (value !== undefined && isJson(value)) result.structured = value
  return result
}

function safeStringify(value: unknown): string {
  try {
    return JSON.stringify(value) ?? String(value)
  } catch {
    return String(value)
  }
}

function validateDefinition(definition: any) {
  if (!definition || typeof definition !== 'object') throw new TypeError('tool definition must be an object')
  if (typeof definition.name !== 'string' || definition.name.length === 0) throw new TypeError('tool definition needs a name')
  if (typeof definition.execute !== 'function') throw new TypeError(`tool \`${definition.name}\` needs an execute function`)
  if (definition.parameters !== undefined && (typeof definition.parameters !== 'object' || definition.parameters === null)) {
    throw new TypeError(`tool \`${definition.name}\` parameters must be a JSON schema object`)
  }
  if (definition.output !== undefined) {
    if (!definition.output || typeof definition.output !== 'object' || typeof definition.output.render !== 'function') {
      throw new TypeError(`tool \`${definition.name}\` output must declare a schema and render function`)
    }
    assertSupportedJsonSchema(definition.output.schema)
  }
  for (const field of ['projectContent', 'finalizeContent', 'timeoutMs']) {
    if (definition[field] !== undefined) throw new TypeError(`tool \`${definition.name}\` requires unsupported DSH execution field \`${field}\``)
  }
  if (definition.deferLoading === true) throw new TypeError(`tool \`${definition.name}\` requires unsupported DSH deferred loading`)
}

function pickPlugin(module: any): any {
  const candidate = module?.default ?? module
  if (typeof candidate === 'function') return candidate
  if (candidate && typeof candidate.apply === 'function') return candidate
  if (module && typeof module.apply === 'function') return module
  throw new Error('entry module exports no Cordis plugin (a function, or an object with `apply`)')
}

/** Names of services that nested fibers under `fiber` are still waiting on. */
function pendingFibers(fiber: any): string[] {
  const out: string[] = []
  const seen = new Set<any>()
  const visit = (current: any) => {
    if (!current || seen.has(current)) return
    seen.add(current)
    if (current.state === 0 /* PENDING */) {
      const names = Object.keys(current.inject ?? {})
      out.push(`${current.name ?? 'plugin'} (${names.join(', ')})`)
    }
  }
  visit(fiber)
  for (const runtime of fiber.ctx?.registry?.values?.() ?? []) {
    for (const child of runtime.fibers ?? []) {
      if (child.parent?.[OWNER] === fiber.ctx?.[OWNER]) visit(child)
    }
  }
  return out
}
