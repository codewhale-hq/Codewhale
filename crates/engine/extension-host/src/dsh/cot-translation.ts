import { Service, type Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'

export type TranslationProvider =
  | 'google'
  | 'bing'
  | 'deepseek-account'
  | 'deepseek-official'

export type TranslationErrorCode =
  | 'TRANSLATION_TEXT_LIMIT'
  | 'TRANSLATION_HTTP_ERROR'
  | 'TRANSLATION_INVALID_RESPONSE'
  | 'TRANSLATION_RESPONSE_LIMIT'
  | 'TRANSLATION_REQUEST_FAILED'
  | 'TRANSLATION_TIMEOUT'
  | 'TRANSLATION_SESSION_REQUIRED'
  | 'TRANSLATION_SESSION_INACTIVE'
  | 'TRANSLATION_STORAGE_ERROR'
  | 'TRANSLATION_UNAVAILABLE'

export interface TranslationRequest {
  text: string
  targetLanguage: string
  sourceLanguage?: string
  provider?: TranslationProvider
  sessionId?: string
}

export interface TranslationSpec {
  text: string
  sourceLanguage: string
  targetLanguage: string
  provider: TranslationProvider
  sessionId?: string
}

export interface TranslatorConfig {
  provider: TranslationProvider
  googleEndpoint: string
  bingEndpoint: string
  timeoutMs: number
  maxTextChars: number
  maxResponseBytes: number
  deepseekTimeoutMs: number
  deepseekMaxOutputTokens: number
}

export interface CotTranslationConfig {
  provider: TranslationProvider
  targetLanguage: string
}

export interface CotTranslationLimits {
  maxTextChars: number
  preferences: CotTranslationConfig
  availableProviders: readonly TranslationProvider[]
}

export interface CredentialsBroker {
  fetch(input: string, init?: RequestInit): Promise<Response>
}

interface TranslatorHostContext extends Context {
  credentials: CredentialsBroker
  llm: unknown
}

const ANONYMOUS_PROVIDERS: readonly TranslationProvider[] = ['bing', 'google']
const VALID_PROVIDERS = new Set<string>([
  'google',
  'bing',
  'deepseek-account',
  'deepseek-official',
])
const LANGUAGE_PREFERENCE_PATTERN = /^(?:auto|[a-zA-Z]{2,8}(?:-[a-zA-Z0-9]{1,8})*)$/
const MAX_CONFIG_TEXT_CHARS = 1048576
const MAX_CONFIG_RESPONSE_BYTES = 8 * 1024 * 1024
const MAX_LANGUAGE_TAG_LENGTH = 64
const MAX_SESSION_ID_LENGTH = 256

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function validateEndpoint(value: string): string {
  const url = new URL(value)
  if (
    !['http:', 'https:'].includes(url.protocol) ||
    url.username !== '' ||
    url.password !== '' ||
    url.hash !== ''
  ) {
    throw new Error(
      'Translation endpoint must be an HTTP(S) URL without credentials or a fragment',
    )
  }
  return url.href
}

function mapLanguage(tag: string, provider: 'google' | 'bing'): string {
  const lower = tag.toLowerCase()
  if (
    lower === 'zh' ||
    lower === 'zh-cn' ||
    lower === 'zh-sg' ||
    lower === 'zh-hans'
  ) {
    return provider === 'google' ? 'zh-CN' : 'zh-Hans'
  }
  if (
    lower === 'zh-tw' ||
    lower === 'zh-hk' ||
    lower === 'zh-mo' ||
    lower === 'zh-hant'
  ) {
    return provider === 'google' ? 'zh-TW' : 'zh-Hant'
  }
  return tag
}

function parseGooglePayload(payload: unknown): string {
  if (
    !Array.isArray(payload) ||
    !Array.isArray(payload[0]) ||
    payload[0].length === 0
  ) {
    throw new TranslationError(
      'TRANSLATION_INVALID_RESPONSE',
      'Google translation returned invalid payload',
    )
  }
  const parts: string[] = []
  for (const segment of payload[0]) {
    if (!Array.isArray(segment) || typeof segment[0] !== 'string') {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Google translation returned invalid payload',
      )
    }
    parts.push(segment[0])
  }
  return parts.join('')
}

function parseBingPayload(payload: unknown): string {
  if (!Array.isArray(payload) || payload.length === 0 || !isRecord(payload[0])) {
    throw new TranslationError(
      'TRANSLATION_INVALID_RESPONSE',
      'Bing translation returned invalid payload',
    )
  }
  const translations = payload[0].translations
  if (
    !Array.isArray(translations) ||
    translations.length === 0 ||
    !isRecord(translations[0]) ||
    typeof translations[0].text !== 'string'
  ) {
    throw new TranslationError(
      'TRANSLATION_INVALID_RESPONSE',
      'Bing translation returned invalid payload',
    )
  }
  return translations[0].text
}

function raceWithSignal<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) {
    promise.catch(() => {})
    return Promise.reject(signal.reason)
  }
  return new Promise<T>((resolve, reject) => {
    let settled = false
    const onAbort = () => {
      if (settled) return
      settled = true
      promise.catch(() => {})
      reject(signal.reason)
    }
    signal.addEventListener('abort', onAbort, { once: true })
    promise.then(
      (value) => {
        if (settled) return
        settled = true
        signal.removeEventListener('abort', onAbort)
        resolve(value)
      },
      (error) => {
        if (settled) return
        settled = true
        signal.removeEventListener('abort', onAbort)
        reject(error)
      },
    )
  })
}

async function readBoundedResponseJson(
  response: Response,
  maxResponseBytes: number,
  provider: 'google' | 'bing',
  signal: AbortSignal,
): Promise<unknown> {
  if (!response.body) {
    throw new TranslationError(
      'TRANSLATION_INVALID_RESPONSE',
      `${provider} translation returned empty body`,
    )
  }
  if (signal.aborted) {
    void response.body.cancel().catch(() => {})
    throw signal.reason
  }
  const chunks: Uint8Array[] = []
  let totalBytes = 0
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined
  let cancelled = false
  const cancelReader = () => {
    if (cancelled || !reader) return
    cancelled = true
    void reader.cancel().catch(() => {})
  }
  const onAbort = () => {
    cancelReader()
  }
  try {
    reader = response.body.getReader()
    signal.addEventListener('abort', onAbort, { once: true })
    if (signal.aborted) {
      cancelReader()
      throw signal.reason
    }
    while (true) {
      const { done, value } = await raceWithSignal(reader.read(), signal)
      if (done) break
      if (!value) continue
      totalBytes += value.byteLength
      if (totalBytes > maxResponseBytes) {
        cancelReader()
        throw new TranslationError(
          'TRANSLATION_RESPONSE_LIMIT',
          `${provider} translation response exceeded size limit`,
        )
      }
      chunks.push(value)
    }
  } catch (error) {
    cancelReader()
    if (signal.aborted) throw signal.reason
    if (error instanceof TranslationError) throw error
    throw new TranslationError(
      'TRANSLATION_REQUEST_FAILED',
      `${provider} translation request failed`,
    )
  } finally {
    signal.removeEventListener('abort', onAbort)
    reader?.releaseLock()
  }
  const merged = new Uint8Array(totalBytes)
  let offset = 0
  for (const chunk of chunks) {
    merged.set(chunk, offset)
    offset += chunk.byteLength
  }
  try {
    return JSON.parse(new TextDecoder('utf-8').decode(merged))
  } catch {
    throw new TranslationError(
      'TRANSLATION_INVALID_RESPONSE',
      `${provider} translation returned invalid JSON`,
    )
  }
}

export class TranslationError extends Error {
  readonly code: TranslationErrorCode

  constructor(code: TranslationErrorCode, message: string) {
    super(message)
    this.name = 'TranslationError'
    this.code = code
  }
}

export class CotTranslationRemoteError extends Error {
  readonly code = 'cotTranslation/failed'
  readonly data: Record<string, never> = {}

  constructor(message = 'Translation failed') {
    super(message)
    this.name = 'CotTranslationRemoteError'
  }
}

export const TranslatorConfigSchema = z.object({
  provider: z
    .union(['google', 'bing', 'deepseek-account', 'deepseek-official'])
    .default('bing'),
  googleEndpoint: z
    .transform(z.string(), validateEndpoint)
    .default('https://translate.googleapis.com/translate_a/single'),
  bingEndpoint: z
    .transform(z.string(), validateEndpoint)
    .default('https://edge.microsoft.com/translate/translatetext'),
  timeoutMs: z.natural().min(1).max(2147483647).default(10000),
  maxTextChars: z.natural().min(2).max(MAX_CONFIG_TEXT_CHARS).default(4000),
  maxResponseBytes: z
    .natural()
    .min(1)
    .max(MAX_CONFIG_RESPONSE_BYTES)
    .default(1048576),
  deepseekTimeoutMs: z.natural().min(1).max(2147483647).default(60000),
  deepseekMaxOutputTokens: z.natural().min(1).max(131072).default(8192),
})

export const CotTranslationConfigSchema = z.object({
  provider: z
    .union(['google', 'bing', 'deepseek-account', 'deepseek-official'])
    .default('bing'),
  targetLanguage: z
    .string()
    .pattern(LANGUAGE_PREFERENCE_PATTERN)
    .default('auto'),
})

export class TranslatorService extends Service {
  static inject = ['credentials', 'llm']
  static provide = 'translator'
  static Config = TranslatorConfigSchema

  readonly config: TranslatorConfig
  private readonly lifetime = new AbortController()
  private readonly pending = new Set<Promise<unknown>>()

  constructor(ctx: Context, config?: Partial<TranslatorConfig>) {
    super(ctx, 'translator')
    this.config = TranslatorConfigSchema(config ?? {}) as TranslatorConfig
    ctx.effect(() => async () => {
      this.lifetime.abort(new Error('Translator service disposed'))
      await Promise.allSettled([...this.pending])
    })
  }

  get maxTextChars(): number {
    return this.config.maxTextChars
  }

  private combineSignal(signal?: AbortSignal): AbortSignal {
    return signal
      ? AbortSignal.any([this.lifetime.signal, signal])
      : this.lifetime.signal
  }

  private assertTextLimit(text: string): void {
    if (typeof text !== 'string') {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Translation text must be a string',
      )
    }
    if (text.length > this.config.maxTextChars) {
      throw new TranslationError(
        'TRANSLATION_TEXT_LIMIT',
        `Translation text exceeds maximum length (${this.config.maxTextChars} characters)`,
      )
    }
  }

  async availableProviders(
    signal?: AbortSignal,
  ): Promise<readonly TranslationProvider[]> {
    const combined = this.combineSignal(signal)
    if (combined.aborted) throw combined.reason
    return [...ANONYMOUS_PROVIDERS]
  }

  resolve(request: TranslationRequest): TranslationSpec {
    if (this.lifetime.signal.aborted) throw this.lifetime.signal.reason
    if (!isRecord(request)) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Translation request must be an object',
      )
    }
    this.assertTextLimit(request.text)
    const provider = request.provider ?? this.config.provider
    if (typeof provider !== 'string' || !VALID_PROVIDERS.has(provider)) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Unsupported translation provider',
      )
    }
    if (
      typeof request.targetLanguage !== 'string' ||
      request.targetLanguage.trim() === '' ||
      request.targetLanguage.length > MAX_LANGUAGE_TAG_LENGTH
    ) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Target language is required',
      )
    }
    const sourceLanguage = request.sourceLanguage ?? 'auto'
    if (
      typeof sourceLanguage !== 'string' ||
      sourceLanguage.trim() === '' ||
      sourceLanguage.length > MAX_LANGUAGE_TAG_LENGTH
    ) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Source language must be a bounded non-empty string',
      )
    }
    if (
      request.sessionId !== undefined &&
      (typeof request.sessionId !== 'string' ||
        request.sessionId.trim() === '' ||
        request.sessionId.length > MAX_SESSION_ID_LENGTH)
    ) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Session ID must be a bounded non-empty string',
      )
    }
    return {
      text: request.text,
      sourceLanguage,
      targetLanguage: request.targetLanguage,
      provider: provider as TranslationProvider,
      ...(request.sessionId !== undefined
        ? { sessionId: request.sessionId }
        : {}),
    }
  }

  async translate(
    spec: TranslationSpec,
    signal?: AbortSignal,
  ): Promise<string> {
    const combined = this.combineSignal(signal)
    if (combined.aborted) throw combined.reason
    if (!isRecord(spec)) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Translation spec must be an object',
      )
    }
    this.assertTextLimit(spec.text)
    if (typeof spec.provider !== 'string' || !VALID_PROVIDERS.has(spec.provider)) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Unsupported translation provider',
      )
    }
    if (
      typeof spec.targetLanguage !== 'string' ||
      spec.targetLanguage.trim() === '' ||
      spec.targetLanguage.length > MAX_LANGUAGE_TAG_LENGTH
    ) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Target language is required',
      )
    }
    if (
      typeof spec.sourceLanguage !== 'string' ||
      spec.sourceLanguage.trim() === '' ||
      spec.sourceLanguage.length > MAX_LANGUAGE_TAG_LENGTH
    ) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Source language must be a bounded non-empty string',
      )
    }
    if (
      spec.sessionId !== undefined &&
      (typeof spec.sessionId !== 'string' ||
        spec.sessionId.trim() === '' ||
        spec.sessionId.length > MAX_SESSION_ID_LENGTH)
    ) {
      throw new TranslationError(
        'TRANSLATION_INVALID_RESPONSE',
        'Session ID must be a bounded non-empty string',
      )
    }
    if (
      spec.provider === 'deepseek-account' ||
      spec.provider === 'deepseek-official'
    ) {
      throw new TranslationError(
        'TRANSLATION_UNAVAILABLE',
        `Paid translation route ${spec.provider} is not offered in this build`,
      )
    }
    if (spec.text === '') {
      return ''
    }
    const provider = spec.provider
    const run = this.executeAnonymous(provider, spec, combined)
    this.pending.add(run)
    try {
      return await run
    } finally {
      this.pending.delete(run)
    }
  }

  private async executeAnonymous(
    provider: 'google' | 'bing',
    spec: TranslationSpec,
    combined: AbortSignal,
  ): Promise<string> {
    const timeoutController = new AbortController()
    const timer = setTimeout(() => {
      timeoutController.abort(
        new TranslationError(
          'TRANSLATION_TIMEOUT',
          `${provider} translation timed out`,
        ),
      )
    }, this.config.timeoutMs)
    const effectiveSignal = AbortSignal.any([
      combined,
      timeoutController.signal,
    ])
    try {
      const target = mapLanguage(spec.targetLanguage, provider)
      const source =
        spec.sourceLanguage === 'auto'
          ? 'auto'
          : mapLanguage(spec.sourceLanguage, provider)

      let url: URL
      let init: RequestInit
      if (provider === 'google') {
        url = new URL(this.config.googleEndpoint)
        url.searchParams.delete('client')
        url.searchParams.delete('sl')
        url.searchParams.delete('tl')
        url.searchParams.delete('dt')
        url.searchParams.delete('q')
        init = {
          method: 'POST',
          headers: { 'content-type': 'application/x-www-form-urlencoded' },
          body: new URLSearchParams({
            client: 'gtx',
            sl: source,
            tl: target,
            dt: 't',
            q: spec.text,
          }).toString(),
          redirect: 'error',
          credentials: 'omit',
          signal: effectiveSignal,
        }
      } else {
        url = new URL(this.config.bingEndpoint)
        if (source === 'auto') {
          url.searchParams.delete('from')
        } else {
          url.searchParams.set('from', source)
        }
        url.searchParams.set('to', target)
        url.searchParams.set('isEnterpriseClient', 'false')
        init = {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify([spec.text]),
          redirect: 'error',
          credentials: 'omit',
          signal: effectiveSignal,
        }
      }

      const hostCtx = this.ctx as unknown as TranslatorHostContext
      const fetchPromise = Promise.resolve().then(() =>
        hostCtx.credentials.fetch(url.href, init),
      )
      fetchPromise.then(
        (res) => {
          if (effectiveSignal.aborted) {
            void res?.body?.cancel?.().catch(() => {})
          }
        },
        () => {},
      )
      let response: Response
      try {
        response = await raceWithSignal(fetchPromise, effectiveSignal)
      } catch (error) {
        if (combined.aborted) throw combined.reason
        if (timeoutController.signal.aborted) {
          throw timeoutController.signal.reason
        }
        if (error instanceof TranslationError) throw error
        throw new TranslationError(
          'TRANSLATION_REQUEST_FAILED',
          `${provider} translation request failed`,
        )
      }

      if (combined.aborted) {
        void response?.body?.cancel?.().catch(() => {})
        throw combined.reason
      }
      if (timeoutController.signal.aborted) {
        void response?.body?.cancel?.().catch(() => {})
        throw timeoutController.signal.reason
      }

      if (response.redirected) {
        if (response.body) {
          await raceWithSignal(
            response.body.cancel().catch(() => {}),
            effectiveSignal,
          ).catch(() => {})
        }
        if (combined.aborted) throw combined.reason
        if (timeoutController.signal.aborted) {
          throw timeoutController.signal.reason
        }
        throw new TranslationError(
          'TRANSLATION_REQUEST_FAILED',
          `${provider} translation response was redirected`,
        )
      }

      if (!response.ok) {
        if (response.body) {
          await raceWithSignal(
            response.body.cancel().catch(() => {}),
            effectiveSignal,
          ).catch(() => {})
        }
        if (combined.aborted) throw combined.reason
        if (timeoutController.signal.aborted) {
          throw timeoutController.signal.reason
        }
        throw new TranslationError(
          'TRANSLATION_HTTP_ERROR',
          `${provider} translation failed (HTTP ${response.status})`,
        )
      }

      const payload = await readBoundedResponseJson(
        response,
        this.config.maxResponseBytes,
        provider,
        effectiveSignal,
      )
      return provider === 'google'
        ? parseGooglePayload(payload)
        : parseBingPayload(payload)
    } finally {
      clearTimeout(timer)
    }
  }
}

export function reviewedCotTranslationModule() {
  return TranslatorService
}

export async function cotTranslationLimits(
  translator: Pick<TranslatorService, 'maxTextChars' | 'availableProviders'>,
  config: Partial<CotTranslationConfig>,
  signal?: AbortSignal,
): Promise<CotTranslationLimits> {
  if (signal?.aborted) throw signal.reason
  const preferences = CotTranslationConfigSchema(
    config ?? {},
  ) as CotTranslationConfig
  const availableProviders = await translator.availableProviders(signal)
  if (signal?.aborted) throw signal.reason
  return {
    maxTextChars: translator.maxTextChars,
    preferences,
    availableProviders,
  }
}

export async function cotTranslationTranslate(
  translator: Pick<TranslatorService, 'resolve' | 'translate'>,
  config: Partial<CotTranslationConfig>,
  request: TranslationRequest,
  signal?: AbortSignal,
): Promise<string> {
  if (signal?.aborted) throw signal.reason
  try {
    const preferences = CotTranslationConfigSchema(
      config ?? {},
    ) as CotTranslationConfig
    const provider = request.provider ?? preferences.provider
    if (provider !== preferences.provider) {
      throw new CotTranslationRemoteError('Translation failed')
    }
    if (
      preferences.targetLanguage !== 'auto' &&
      request.targetLanguage !== preferences.targetLanguage
    ) {
      throw new CotTranslationRemoteError('Translation failed')
    }
    const spec = translator.resolve({
      ...request,
      provider,
    })
    return await translator.translate(spec, signal)
  } catch (error) {
    if (signal?.aborted) throw signal.reason
    throw new CotTranslationRemoteError('Translation failed')
  }
}
