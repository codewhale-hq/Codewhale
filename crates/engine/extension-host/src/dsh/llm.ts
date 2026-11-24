import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import type { AdapterHost, DshAdapter } from './adapters.ts'

export const SESSION_PROVIDER = 'session'

export interface BrokeredModel {
  provider: string
  id: string
  name: string
  context_window: number | null
  default_max_output_tokens: number
  max_output_tokens: number
}

export interface BrokeredCatalog {
  providers: { id: string, name: string }[]
  models: BrokeredModel[]
  limits: Record<string, number>
}

export interface BrokeredUsage {
  coverage: string
  input_tokens: number | null
  output_tokens: number | null
  cache_read_tokens: number | null
  cache_write_tokens: number | null
  reasoning_tokens: number | null
}

export interface BrokeredCompletion {
  text: string
  finish: string
  route: { provider: string, model: string } | null
  usage: BrokeredUsage
  receipt: { requests: number, duration_ms: number }
}

interface StreamOptions {
  provider?: unknown
  model?: unknown
  messages?: unknown
  system?: unknown
  tools?: unknown
  toolHistory?: unknown
  maxTokens?: unknown
  signal?: AbortSignal
}

type Chunk =
  | { type: 'block-start', index: number, blockType: 'text' }
  | { type: 'text-delta', index: number, text: string }
  | { type: 'block-end', index: number, block: { type: 'text', text: string } }
  | { type: 'usage', usage: { inputTokens: number, outputTokens: number, totalTokens: number, cacheReadTokens?: number, cacheWriteTokens?: number, reasoningTokens?: number } }
  | { type: 'finish', reason: { kind: 'stop' } | { kind: 'aborted' | 'error', failure: { message: string, code: string } } }

const invalid = (message: string) => new RpcError(ErrorCode.InvalidParams, message)
const notBrokered = (method: string): never => {
  throw new RpcError(ErrorCode.NotAvailable, `llm.${method} is not available: Codewhale brokers model calls on the session's approved route and registers no plugin adapters, providers or keys`)
}

function catalogOf(value: unknown): BrokeredCatalog {
  const catalog = value as Partial<BrokeredCatalog> | null
  if (catalog === null || typeof catalog !== 'object' || !Array.isArray(catalog.providers) || !Array.isArray(catalog.models)
    || catalog.models.some((model) => model === null || typeof model !== 'object' || typeof model.id !== 'string' || typeof model.provider !== 'string')) {
    throw new RpcError(ErrorCode.Internal, 'invalid llm broker catalog')
  }
  return catalog as BrokeredCatalog
}

function completionOf(value: unknown): BrokeredCompletion {
  const completion = value as Partial<BrokeredCompletion> | null
  if (completion === null || typeof completion !== 'object' || typeof completion.text !== 'string'
    || typeof completion.finish !== 'string' || completion.finish.length === 0
    || completion.usage === null || typeof completion.usage !== 'object' || typeof completion.usage.coverage !== 'string') {
    throw new RpcError(ErrorCode.Internal, 'invalid llm broker completion')
  }
  return completion as BrokeredCompletion
}

function textOf(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) throw invalid('the llm broker takes text message content')
  return content.map((block) => {
    if (block === null || typeof block !== 'object' || block.type !== 'text' || typeof block.text !== 'string') {
      throw invalid('the llm broker takes text blocks only; images, files and tool calls are not brokered')
    }
    return block.text as string
  }).join('')
}

export function promptOf(messages: unknown): string {
  if (!Array.isArray(messages) || messages.length === 0) throw invalid('the llm broker needs at least one message')
  const turns = messages.map((message) => {
    if (message === null || typeof message !== 'object' || typeof message.role !== 'string') throw invalid('each message has a role and content')
    if (message.role === 'tool') throw invalid('tool results are not brokered')
    return { role: message.role as string, text: textOf(message.content) }
  })
  const only = turns.length === 1 ? turns[0] : undefined
  if (only !== undefined && only.role === 'user') return only.text
  return turns.map((turn) => `${turn.role}: ${turn.text}`).join('\n\n')
}

function modelInfo(model: BrokeredModel) {
  return {
    provider: model.provider,
    id: model.id,
    name: model.name,
    inputModalities: ['text'] as const,
    ...(typeof model.context_window === 'number' ? { context: { contextWindow: model.context_window } } : {}),
    defaultMaxTokens: model.default_max_output_tokens,
  }
}

function usageChunk(usage: BrokeredUsage): Chunk | undefined {
  if (usage.coverage !== 'reported' || typeof usage.input_tokens !== 'number' || typeof usage.output_tokens !== 'number') return undefined
  return {
    type: 'usage',
    usage: {
      inputTokens: usage.input_tokens,
      outputTokens: usage.output_tokens,
      totalTokens: usage.input_tokens + usage.output_tokens,
      ...(typeof usage.cache_read_tokens === 'number' ? { cacheReadTokens: usage.cache_read_tokens } : {}),
      ...(typeof usage.cache_write_tokens === 'number' ? { cacheWriteTokens: usage.cache_write_tokens } : {}),
      ...(typeof usage.reasoning_tokens === 'number' ? { reasoningTokens: usage.reasoning_tokens } : {}),
    },
  }
}

export const llmAdapter: DshAdapter = {
  service: 'llm',
  define(host: AdapterHost) {
    class LlmService extends Service {
      constructor(ctx: any) { super(ctx, 'llm') }

      async catalog(): Promise<BrokeredCatalog> {
        return catalogOf(await host.broker(this.ctx, 'llm', 'catalog', {}))
      }

      async complete(input: { prompt: string, system?: string, provider?: string, model?: string, maxTokens?: number, timeoutMs?: number }): Promise<BrokeredCompletion> {
        if (input === null || typeof input !== 'object' || typeof input.prompt !== 'string') throw invalid('llm.complete takes { prompt }')
        if (input.system !== undefined && typeof input.system !== 'string') throw invalid('system must be a string')
        if (input.provider !== undefined && typeof input.provider !== 'string') throw invalid('provider must be a string')
        if (input.model !== undefined && typeof input.model !== 'string') throw invalid('model must be a string')
        if (input.maxTokens !== undefined && (typeof input.maxTokens !== 'number' || !Number.isInteger(input.maxTokens) || input.maxTokens <= 0 || input.maxTokens > 0xffff_ffff)) {
          throw invalid('maxTokens must be a positive integer')
        }
        if (input.timeoutMs !== undefined && (typeof input.timeoutMs !== 'number' || !Number.isSafeInteger(input.timeoutMs) || input.timeoutMs <= 0)) {
          throw invalid('timeoutMs must be a positive integer')
        }
        return completionOf(await host.broker(this.ctx, 'llm', 'complete', {
          prompt: input.prompt,
          ...(input.system !== undefined ? { system: input.system } : {}),
          ...(input.provider !== undefined ? { provider: input.provider } : {}),
          ...(input.model !== undefined ? { model: input.model } : {}),
          ...(input.maxTokens !== undefined ? { max_output_tokens: input.maxTokens } : {}),
          ...(input.timeoutMs !== undefined ? { timeout_ms: input.timeoutMs } : {}),
        }))
      }

      async listModels(provider: unknown) {
        const catalog = await this.catalog()
        return catalog.models.filter((model) => model.provider === provider).map(modelInfo)
      }

      async resolveModelInfo(provider: unknown, model: unknown) {
        const found = (await this.catalog()).models.find((candidate) => candidate.provider === provider && candidate.id === model)
        if (found === undefined) throw new RpcError(ErrorCode.NotAvailable, 'the llm broker serves only the models in its catalog')
        return modelInfo(found)
      }

      async* stream(options: StreamOptions): AsyncGenerator<Chunk> {
        if (options === null || typeof options !== 'object') throw invalid('llm.stream takes generate options')
        if ((options.tools !== undefined && (!Array.isArray(options.tools) || options.tools.length > 0)) || options.toolHistory !== undefined) throw invalid('the llm broker runs text completions without tools')
        const prompt = promptOf(options.messages)
        if (options.system !== undefined && typeof options.system !== 'string') throw invalid('system must be a string')
        if (options.provider !== undefined && typeof options.provider !== 'string') throw invalid('provider must be a string')
        if (options.model !== undefined && typeof options.model !== 'string') throw invalid('model must be a string')
        if (options.maxTokens !== undefined && (typeof options.maxTokens !== 'number' || !Number.isInteger(options.maxTokens) || options.maxTokens <= 0 || options.maxTokens > 0xffff_ffff)) {
          throw invalid('maxTokens must be a positive integer')
        }
        const isAborted = () => options.signal?.aborted === true
        const aborted = (): Chunk => ({ type: 'finish', reason: { kind: 'aborted', failure: { message: 'the model call was aborted', code: 'ABORTED' } } })
        if (isAborted()) {
          yield aborted()
          return
        }
        let completion: BrokeredCompletion
        try {
          completion = await this.complete({
            prompt,
            ...(options.system !== undefined ? { system: options.system } : {}),
            ...(options.provider !== undefined ? { provider: options.provider } : {}),
            ...(options.model !== undefined ? { model: options.model } : {}),
            ...(options.maxTokens !== undefined ? { maxTokens: options.maxTokens } : {}),
          })
        } catch (error) {
          if (isAborted()) {
            yield aborted()
            return
          }
          const message = error instanceof Error ? error.message : 'the model call did not complete'
          yield { type: 'finish', reason: { kind: 'error', failure: { message, code: 'BROKER_REFUSED' } } }
          return
        }
        if (isAborted()) {
          yield aborted()
          return
        }
        yield { type: 'block-start', index: 0, blockType: 'text' }
        yield { type: 'text-delta', index: 0, text: completion.text }
        yield { type: 'block-end', index: 0, block: { type: 'text', text: completion.text } }
        const usage = usageChunk(completion.usage)
        if (usage !== undefined) yield usage
        yield { type: 'finish', reason: { kind: 'stop' } }
      }

      listProviders(): never { return notBrokered('listProviders') }
      registerAdapter(): never { return notBrokered('registerAdapter') }
      registerConfigurableProviders(): never { return notBrokered('registerConfigurableProviders') }
      listConfigurableProviders(): never { return notBrokered('listConfigurableProviders') }
      registerModelDiscovery(): never { return notBrokered('registerModelDiscovery') }
      discoverModels(): never { return notBrokered('discoverModels') }
      prepareCall(): never { return notBrokered('prepareCall') }
      resolveCallConfig(): never { return notBrokered('resolveCallConfig') }
    }
    Object.freeze(LlmService.prototype)
    return LlmService
  },
}
