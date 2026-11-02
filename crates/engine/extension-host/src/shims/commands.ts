/**
 * The `commands` service: slash commands a plugin contributes.
 *
 * Two audiences share one `register`:
 *
 * - **Codewhale plugins** write
 *   `ctx.commands.register({ name, description, argumentHint?, handler })`.
 * - **DSH plugins** (`@deepseek-ai/dsh-commands`) write
 *   `ctx.commands.register({ name, description, input?: { hint }, handler })`
 *   and return `{ kind: 'success' | 'error', text }`. That surface is
 *   accepted as is.
 *
 * `register` returns an idempotent disposer, like `tools.register`; a
 * registration is an effect of the calling plugin's fiber, so unloading the
 * plugin removes it. The core admits or refuses each one (`registry/register`
 * with `kind: 'command'`). A plugin command colliding with a built-in gets a
 * plugin-specific name; unavailable names are refused with a reason.
 *
 * A handler is called only when the *user* runs the command, and it can only
 * return an answer; it cannot call the model, a tool or approval. Results:
 *
 * - a string, or `{ kind: 'success', text? }`: shown to the user;
 * - `{ kind: 'error', text }`: shown as a failure (a thrown error is too);
 * - `{ kind: 'submit', prompt, text? }` (Codewhale only): `prompt` becomes the
 *   user's next message and runs through the normal turn, tool approval
 *   included; `text` is shown beside it.
 *
 * The invocation carries `args` (what follows the name, trimmed), `rawInput`
 * (DSH's spelling: the text after the name including its leading separator,
 * so `' hello'`), `commandId`, `signal` (aborted when the core cancels the
 * call), an always-empty `attachments`, and the read-only strings `workspace`
 * (where the user ran the command) and `dataDir` (the plugin's own writable
 * directory). With an authoritative caller session, `agent` exposes its id
 * and one text-only followup captured during this command and returned as
 * Submit after successful completion. It is sealed on settlement or cancel.
 * No general agent/session service is provided. Not provided: attachments (`input.attachments`
 * is refused), `recordInput`/`definitionId` (accepted, ignored: Core owns
 * command receipts), `list`/`find`/`execute`, and `sourceEventSeq`
 * (accepted in a result, ignored).
 */
import { Service } from '@deepseek-ai/cordis'
import type { CommandResultWire } from '../protocol.ts'
import { isJson } from '../json.ts'
import type { OwnedEntry, OwnerBase } from './owned.ts'

/** DSH's command grammar. The core enforces the same one. */
const COMMAND_NAME = /^[a-z][a-z0-9_-]*$/u
const NO_ATTACHMENTS: readonly never[] = Object.freeze([])

/** A registration, from this host's side. */
export interface LocalCommand<O extends OwnerBase = OwnerBase> extends OwnedEntry<O> {
  definition: NormalizedCommand
}

export interface NormalizedCommand {
  name: string
  description: string
  argumentHint?: string
  handler: (invocation: CommandInvocation) => unknown
}

export interface CommandInvocation {
  readonly commandId: string
  /** What follows the command name, trimmed. */
  readonly args: string
  /** DSH spelling: what follows the name, with its leading separator. */
  readonly rawInput: string
  readonly attachments: readonly never[]
  readonly signal: AbortSignal
  /** The workspace the user ran the command in (absent if the core did not say). */
  readonly workspace?: string
  /** This plugin's own writable directory. */
  readonly dataDir?: string
  readonly sessionId?: string
  readonly agentId?: string
  readonly originTurnId?: string
  readonly agent: Readonly<{ id: string; followup(message: unknown): void }>
}

/** Reject an invalid definition before it reaches the core, with a message that names the problem. */
export function normalizeCommand(definition: any): NormalizedCommand {
  if (!definition || typeof definition !== 'object') throw new TypeError('command definition must be an object')
  const { name } = definition
  if (typeof name !== 'string' || !COMMAND_NAME.test(name)) {
    throw new TypeError(`command name ${JSON.stringify(name)} must match ${String(COMMAND_NAME)}`)
  }
  if (typeof definition.description !== 'string' || definition.description.trim().length === 0) {
    throw new TypeError(`command "${name}" needs a non-empty description`)
  }
  if (typeof definition.handler !== 'function') throw new TypeError(`command "${name}" handler must be a function`)
  let hint: unknown = definition.argumentHint
  const input: unknown = definition.input
  if (input !== undefined) {
    if (typeof input !== 'object' || input === null || typeof (input as any).hint !== 'string') {
      throw new TypeError(`command "${name}" input hint must be a string`)
    }
    if ((input as any).attachments === true) {
      throw new TypeError(`command "${name}": attachments are not supported by the Codewhale extension host`)
    }
    hint ??= (input as any).hint
  }
  if (hint !== undefined && (typeof hint !== 'string' || hint.trim().length === 0)) {
    throw new TypeError(`command "${name}" argument hint must be a non-empty string`)
  }
  return {
    name,
    description: definition.description,
    ...(hint === undefined ? {} : { argumentHint: hint as string }),
    handler: definition.handler,
  }
}

/** What `registry/register` carries for a command. */
export function commandSpec(command: NormalizedCommand) {
  return {
    name: command.name,
    description: command.description,
    ...(command.argumentHint === undefined ? {} : { argument_hint: command.argumentHint }),
  }
}

export function makeInvocation(
  command: string,
  args: string,
  commandId: string,
  signal: AbortSignal,
  context: { workspace?: string; dataDir?: string; sessionId?: string; agentId?: string; originTurnId?: string } = {},
) {
  let open = !signal.aborted
  let prompt: string | undefined
  let failure: Error | undefined
  const close = () => {
    open = false
    prompt = undefined
    signal.removeEventListener('abort', close)
  }
  signal.addEventListener('abort', close, { once: true })
  const reject: (reason: string) => never = (reason) => {
    failure ??= new TypeError(`command "${command}" followup ${reason}`)
    throw failure
  }
  const agent = context.sessionId?.trim() ? Object.freeze({
    id: context.agentId ?? context.sessionId,
    followup(message: unknown) {
      if (!open || signal.aborted) reject('is unavailable after command settlement or cancellation')
      if (failure) throw failure
      if (prompt !== undefined) reject('accepts exactly one message per invocation')
      if (!isJson(message) || message === null || Array.isArray(message) || typeof message !== 'object') {
        reject('requires a lossless JSON user message')
      }
      if (Object.keys(message).some(key => !['id', 'role', 'content', 'source'].includes(key))
        || typeof message.id !== 'string' || message.id.trim() === '' || message.id.length > 256
        || message.role !== 'user') reject('requires only id, user role, content and source fields')
      const source = message.source
      if (!source || Array.isArray(source) || typeof source !== 'object'
        || Object.keys(source).some(key => key !== 'kind') || typeof source.kind !== 'string'
        || source.kind.trim() === '' || source.kind.length > 128) reject('requires a source with one bounded kind')
      const content = message.content
      if (!Array.isArray(content) || content.length !== 1) reject('requires exactly one text block')
      const block = content[0]
      if (!block || Array.isArray(block) || typeof block !== 'object'
        || Object.keys(block).some(key => key !== 'type' && key !== 'text')
        || block.type !== 'text' || typeof block.text !== 'string') reject('requires exactly one plain text block')
      if (block.text.trim() === '' || Buffer.byteLength(block.text, 'utf8') > 128 * 1024) reject('text must be non-empty and at most 128 KiB')
      prompt = block.text
    },
  }) : undefined
  const invocation = Object.freeze(Object.defineProperty({
    commandId,
    args,
    rawInput: args === '' ? '' : ` ${args}`,
    attachments: NO_ATTACHMENTS,
    signal,
    ...context,
  }, 'agent', { get() {
    if (!agent) reject('requires an authoritative caller session')
    return agent
  } })) as CommandInvocation
  return {
    invocation,
    close,
    complete(value: unknown): CommandResultWire {
      if (!open || signal.aborted) reject('is unavailable after command settlement or cancellation')
      if (failure) throw failure
      const result = normalizeResult(command, value)
      if (prompt === undefined || result.kind === 'error') return result
      if (result.kind === 'submit') reject('conflicts with an explicit Submit result')
      return { kind: 'submit', prompt, ...(result.text === undefined ? {} : { text: result.text }) }
    },
  }
}

/** Validate and detach whatever a handler returned. */
export function normalizeResult(command: string, value: unknown): CommandResultWire {
  if (value === undefined || value === null) return { kind: 'success' }
  if (typeof value === 'string') return { kind: 'success', text: value }
  if (typeof value !== 'object') throw new TypeError(`command "${command}" handler must return a result object or a string`)
  const result = value as { kind?: unknown; text?: unknown; prompt?: unknown }
  const text = result.text
  if (text !== undefined && typeof text !== 'string') {
    throw new TypeError(`command "${command}" result text must be a string when supplied`)
  }
  switch (result.kind) {
    case 'success':
      return text === undefined ? { kind: 'success' } : { kind: 'success', text }
    case 'error':
      if (typeof text !== 'string' || text.trim().length === 0) {
        throw new TypeError(`command "${command}" error text must be a non-empty string`)
      }
      return { kind: 'error', text }
    case 'submit':
      if (typeof result.prompt !== 'string' || result.prompt.trim().length === 0) {
        throw new TypeError(`command "${command}" submit prompt must be a non-empty string`)
      }
      return text === undefined ? { kind: 'submit', prompt: result.prompt } : { kind: 'submit', prompt: result.prompt, text }
    default:
      throw new TypeError(`command "${command}" returned unknown result kind ${JSON.stringify(result.kind)}`)
  }
}

/** The host's side of the shim: how a `register` call becomes a core registration. */
export interface CommandsHost<O extends OwnerBase> {
  ownerOf(ctx: any): O | undefined
  addCommand(owner: O, command: NormalizedCommand): () => void
}

/**
 * Build the `commands` service class for `host`. The class is frozen so one
 * plugin cannot rewrite `register` for the others (the owner token is not a
 * boundary between plugins that share the process; design §4.4).
 */
export function defineCommandsService<O extends OwnerBase>(host: CommandsHost<O>) {
  class CommandsShim extends Service {
    constructor(ctx: any) {
      super(ctx, 'commands')
    }

    /** `ctx.commands.register(definition)`: returns an idempotent disposer. */
    register(definition: unknown): () => void {
      const ctx: any = this.ctx
      const owner = host.ownerOf(ctx)
      if (!owner) throw new Error('commands.register called outside an extension owner')
      const command = normalizeCommand(definition)
      return ctx.effect(() => host.addCommand(owner, command), `commands.register(${JSON.stringify(command.name)})`)
    }
  }
  Object.freeze(CommandsShim.prototype)
  return CommandsShim
}
