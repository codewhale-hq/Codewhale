/** Pure transformations of Rust-captured data. No I/O, process, credential or Engine API. */
import type { Json } from '../../protocol.ts'
import { transformWebSnapshot } from './web-adapters.ts'
import { transformGithubSnapshot } from './github-adapter.ts'
import { isReviewOperation, REVIEW_LIMIT, reviewEnvelopeBytes, transformReviewSnapshot } from './review-adapter.ts'

interface Result { ok: true; result: { content: string; success: boolean; metadata: Json } }
type Row = Record<string, unknown>
const MAX_SNAPSHOT = 1024 * 1024

function row(value: unknown): Row {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('invalid adapter snapshot')
  return value as Row
}
function text(value: unknown): string {
  if (typeof value !== 'string') throw new Error('invalid adapter text')
  return value
}
function optionalText(value: unknown): string | undefined {
  return value === null || value === undefined ? undefined : text(value)
}
function optionalNumber(value: unknown): number | undefined {
  if (value === null || value === undefined) return undefined
  if (typeof value !== 'number' || !Number.isFinite(value)) throw new Error('invalid captured numeric field')
  return value
}
// Preserve Rust f64 values, negative zero, derived nonfinite values and exact i64
// timestamps across JSON. Rust decodes these strings into its original DTO.
function float(value: number): string {
  if (Object.is(value, -0)) return '-0'
  if (value === Infinity) return 'inf'
  if (value === -Infinity) return '-inf'
  return String(value)
}
function timestamp(value: unknown): string | undefined {
  const time = optionalText(value)
  if (time === undefined) return undefined
  if (!/^-?\d{1,19}$/.test(time)) throw new Error('invalid captured timestamp')
  const integer = BigInt(time)
  if (integer < -9223372036854775808n || integer > 9223372036854775807n) throw new Error('captured timestamp exceeds i64')
  return time
}
function result(success: boolean, metadata: Json, content = ''): Result {
  return { ok: true, result: { success, content, metadata } }
}
function failure(endpoint: string, kind: 'not_found' | 'upstream', detail: string): Result {
  return result(false, { endpoint, kind, detail })
}

function asciiLower(value: string): string { return value.replace(/[A-Z]/g, letter => letter.toLowerCase()) }

function finance(operation: string, input: Row): Result {
  const request = row(input.request)
  const requested = text(request.requested_ticker)
  const symbol = text(request.resolved_symbol)
  const parsed = row(input.parsed)
  const chart = operation === 'finance_chart'
  const source = chart ? 'yahoo_chart' : 'yahoo_quote'
  let quote: Row
  if (chart) {
    const body = row(parsed.chart)
    if (body.error !== null && body.error !== undefined) {
      const error = row(body.error)
      const description = optionalText(error.description) ?? 'chart endpoint returned an error'
      const code = optionalText(error.code)
      const notFound = (code === undefined ? undefined : asciiLower(code)) === 'not found' || asciiLower(description).includes('not found') || asciiLower(description).includes('symbol may be delisted')
      return failure(source, notFound ? 'not_found' : 'upstream', description)
    }
    if (body.result !== null && body.result !== undefined && !Array.isArray(body.result)) throw new Error('invalid captured chart results')
    const entries = body.result as unknown[] | null | undefined
    if (!entries?.length) return failure(source, 'not_found', `no chart data for symbol '${symbol}'`)
    quote = row(row(entries[0]).meta)
  } else {
    const body = row(parsed.quoteResponse)
    if (!Array.isArray(body.result)) throw new Error('invalid captured quote results')
    const candidate = body.result.map(row).find(value => asciiLower(text(value.symbol)) === asciiLower(symbol))
    if (!candidate) return failure(source, 'not_found', `no result for symbol '${symbol}'`)
    quote = candidate
  }
  const price = optionalNumber(quote.regularMarketPrice)
  if (price === undefined) return failure(source, 'upstream', 'response missing regularMarketPrice')
  const previous = chart
    ? optionalNumber(quote.chartPreviousClose) ?? optionalNumber(quote.previousClose)
    : optionalNumber(quote.regularMarketPreviousClose)
  const computedChange = previous === undefined ? undefined : price - previous
  const computedPercent = previous === undefined || Math.abs(previous) < Number.EPSILON ? undefined : ((price - previous) / previous) * 100
  const change = chart ? computedChange : optionalNumber(quote.regularMarketChange) ?? computedChange
  const percent = chart ? computedPercent : optionalNumber(quote.regularMarketChangePercent) ?? computedPercent
  const name = optionalText(quote.longName) ?? optionalText(quote.shortName)
  const currency = optionalText(quote.currency)
  const state = chart ? undefined : optionalText(quote.marketState)
  const type = optionalText(chart ? quote.instrumentType : quote.quoteType)
  const exchange = optionalText(quote.fullExchangeName) ?? optionalText(chart ? quote.exchangeName : quote.exchange)
  const time = timestamp(quote.regularMarketTime)
  const value: Record<string, Json> = {
    requested_ticker: requested, ticker: text(quote.symbol), price: float(price), source, fallback_used: chart,
    ...(name === undefined ? {} : { name }), ...(currency === undefined ? {} : { currency }),
    ...(change === undefined ? {} : { change: float(change) }),
    ...(percent === undefined ? {} : { change_percent: float(percent) }),
    ...(previous === undefined ? {} : { previous_close: float(previous) }),
    ...(state === undefined ? {} : { market_state: state }), ...(type === undefined ? {} : { quote_type: type }),
    ...(exchange === undefined ? {} : { exchange }), ...(time === undefined ? {} : { market_time: time }),
  }
  return result(true, value)
}

function summary(value: unknown, format: 'json' | 'toml'): Json {
  const descriptor = row(value)
  const kind = text(descriptor.kind)
  const accepted = format === 'json'
    ? ['object', 'array', 'string', 'number', 'boolean', 'null']
    : ['table', 'array', 'string', 'integer', 'float', 'boolean', 'datetime']
  if (!accepted.includes(kind)) throw new Error('invalid captured parser kind')
  if (kind === 'object' || kind === 'table') {
    if (!Array.isArray(descriptor.keys) || descriptor.keys.some(key => typeof key !== 'string')) throw new Error('invalid captured parser keys')
    // Core captured the parser's actual iteration order, including builds
    // with serde_json preserve_order. Keep that exact preview order.
    const keys = descriptor.keys as string[]
    if (new Set(keys).size !== keys.length) throw new Error('duplicate captured parser keys')
    return { top_level: kind, entries: keys.length, keys_preview: keys.slice(0, 10) }
  }
  if (kind === 'array') {
    if (!Number.isSafeInteger(descriptor.entries) || (descriptor.entries as number) < 0) throw new Error('invalid captured array count')
    return { top_level: kind, entries: descriptor.entries as number }
  }
  return { top_level: kind }
}

function data(input: Row): Result {
  const format = text(input.format)
  if (!['auto', 'json', 'toml'].includes(format)) throw new Error('invalid captured data format')
  const source = text(input.source)
  const extension = optionalText(input.extension)
  const requested = format === 'auto' && (extension === 'json' || extension === 'toml') ? extension : format
  const json = row(input.json)
  const toml = row(input.toml)
  if (typeof json.ok !== 'boolean' || typeof toml.ok !== 'boolean') throw new Error('invalid captured parser result')
  const use = (parser: Row, selected: 'json' | 'toml'): Result => {
    if (parser.ok) return result(true, { valid: true, format: selected, source, summary: summary(parser.value, selected) })
    const error = text(parser.error)
    return result(false, { valid: false, format: selected, source, error }, `Invalid ${selected.toUpperCase()}: ${error}`)
  }
  if (requested === 'json') return use(json, 'json')
  if (requested === 'toml') return use(toml, 'toml')
  if (json.ok) return use(json, 'json')
  if (toml.ok) return use(toml, 'toml')
  return result(false, { valid: false, format: 'auto', source, json_error: text(json.error), toml_error: text(toml.error) }, 'Validation failed in auto mode: content is neither valid JSON nor TOML.')
}

// Unicode White_Space is Rust str::trim's contract. JS trim additionally removes
// BOM and omits NEL, which changes speech instructions on saved transcripts.
function rustTrim(value: string): string { return value.replace(/^[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+|[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$/g, '') }
function bool(value: unknown): boolean { if (typeof value !== 'boolean') throw new Error('invalid captured boolean'); return value }
function speech(operation: string, input: Row): Result {
  const surface = text(input.surface)
  if (surface !== 'tool' && surface !== 'cli') throw new Error('invalid speech surface')
  const tool = surface === 'tool'
  const invalid = (error: string) => result(false, { error })
  if (operation === 'speech_format') {
    const raw = text(input.format)
    const format = asciiLower(rustTrim(raw))
    if (format === 'wav' || format === 'mp3' || format === 'pcm16' || format === 'pcm') return result(true, { format: format === 'pcm' ? 'pcm16' : format })
    return invalid(tool ? `unsupported speech format '${raw}' (allowed: wav, mp3, pcm16)` : `Unsupported speech format '${raw}' (allowed: wav, mp3, pcm16)`)
  }
  const modelHint = optionalText(input.model)
  const hasClone = bool(input.has_clone_path)
  const hasVoice = bool(input.has_voice)
  const nonemptyVoice = bool(input.voice_nonempty)
  const dataUri = bool(input.voice_is_data_uri)
  if ((nonemptyVoice || dataUri) && !hasVoice) throw new Error('inconsistent captured voice presence')
  const instructionInput = optionalText(input.instruction)
  const promptInput = optionalText(input.voice_prompt)
  if (hasClone && hasVoice) return invalid(tool ? 'use either clone_voice or voice for cloned voice data, not both' : 'Use either --clone-voice or --voice for cloned voice data, not both')
  const model = modelHint ?? ((hasClone || dataUri) ? 'mimo-v2.5-tts-voiceclone' : promptInput !== undefined ? 'mimo-v2.5-tts-voicedesign' : 'mimo-v2.5-tts')
  const lower = asciiLower(model)
  if (!lower.includes('tts')) {
    const examples = 'mimo-v2.5-tts, mimo-v2.5-tts-voicedesign, mimo-v2.5-tts-voiceclone, mimo-v2-tts'
    return invalid(tool ? `speech tool requires a TTS model (examples: ${examples}), got '${model}'` : `speech requires a TTS model (examples: ${examples}); got ${model}`)
  }
  const parts = [promptInput, instructionInput].map(value => value === undefined ? undefined : rustTrim(value)).filter((value): value is string => value !== undefined && value !== '')
  const instruction = parts.length ? parts.join('\n\n') : null
  if (lower.includes('voicedesign') && instruction === null) return invalid(tool ? 'mimo-v2.5-tts-voicedesign requires voice_prompt or instruction' : 'mimo-v2.5-tts-voicedesign requires --voice-prompt or --instruction to describe the voice')
  let voice: string
  if (hasClone) voice = 'clone'
  else if (lower.includes('voicedesign')) voice = 'omit'
  else if (nonemptyVoice) voice = 'raw'
  else if (lower.includes('voiceclone')) return invalid(tool ? 'mimo-v2.5-tts-voiceclone requires clone_voice <mp3|wav> or voice <data-uri>' : 'mimo-v2.5-tts-voiceclone requires --clone-voice <mp3|wav> or --voice <data-uri>')
  else voice = 'default'
  return result(true, { model, instruction, voice })
}

function pdfProjection(input: Row): Result {
  const state = text(input.state)
  const decision = (code: string, message?: string) => result(true, { kind: 'pdf_decision', code, ...(message === undefined ? {} : { message }) })
  if (state !== 'complete') {
    if (!['binary_unavailable', 'cancelled', 'timed_out', 'execution'].includes(state)) throw new Error('invalid PDF process state')
    return decision(state, text(input.message))
  }
  const success = bool(input.success)
  const stdoutOverflow = bool(input.stdout_truncated)
  const stderrOverflow = bool(input.stderr_truncated)
  const stderr = text(input.stderr)
  const code = input.exit_code
  if (code !== null && (!Number.isInteger(code) || (code as number) < -2147483648 || (code as number) > 2147483647)) throw new Error('invalid PDF exit code')
  if (stdoutOverflow) return decision('execution', 'pdftotext output exceeded the 16777216 byte safety limit')
  if (!success) return decision('execution', `pdftotext failed (exit ${code === null ? 'None' : `Some(${code})`}): ${stderr || 'no diagnostic output'}${stderrOverflow ? ' [truncated]' : ''}`)
  return decision('success')
}

function ocrProjection(input: Row): Result {
  const decision = (code: string, trim_end = false, message?: string) => result(true, { kind: 'ocr_decision', code, trim_end, ...(message === undefined ? {} : { message }) })
  const state = text(input.state), status = text(input.status)
  if (state === 'native') {
    if (Object.keys(input).some(key => !['kind','state','status','can_fallback','next_ticket'].includes(key)) || !['success','error','unavailable'].includes(status)) throw new Error('invalid OCR Native projection')
    const fallback = bool(input.can_fallback)
    if (status === 'success') {
      if (fallback || input.next_ticket !== undefined) throw new Error('successful OCR Native step cannot launch fallback')
      return decision('native_success')
    }
    if (fallback) {
      if (typeof input.next_ticket !== 'string' || input.next_ticket.length < 1 || input.next_ticket.length > 256) throw new Error('OCR fallback has no exact continuation grant')
      return decision('fallback')
    }
    if (input.next_ticket !== undefined) throw new Error('OCR fallback is not admitted')
    if (status === 'error') return decision('native_error')
    return decision('no_backend', false, 'image_ocr: no local OCR backend is available. On macOS, update to a version with the Vision framework; on Linux/Windows install tesseract and restart codewhale.')
  }
  if (state !== 'tesseract') throw new Error('invalid OCR process stage')
  if (status === 'fault') {
    if (Object.keys(input).some(key => !['kind','state','status'].includes(key))) throw new Error('invalid OCR fault projection')
    return decision('fault')
  }
  if (status !== 'complete' || Object.keys(input).some(key => !['kind','state','status','success','exit_code'].includes(key))) throw new Error('invalid OCR Tesseract projection')
  const success = bool(input.success), code = input.exit_code
  if (code !== null && (!Number.isInteger(code) || (code as number) < -2147483648 || (code as number) > 2147483647)) throw new Error('invalid OCR exit code')
  return success ? decision('tesseract_success', true) : decision('execution', false, `tesseract failed (exit ${code === null ? 'None' : `Some(${code})`}): `)
}

export function transformStockSnapshot(value: unknown): Result {
  const snapshot = row(value)
  const review = snapshot.kind === 'stock_adapter' && isReviewOperation(snapshot.operation)
  if (review ? reviewEnvelopeBytes(snapshot) > REVIEW_LIMIT : Buffer.byteLength(JSON.stringify(value)) > MAX_SNAPSHOT) throw new Error(review ? 'serialized review snapshot/envelope exceeds 16 MiB' : 'adapter snapshot exceeds 1 MiB')
  if (snapshot.kind === 'ocr_process') return ocrProjection(snapshot)
  if (snapshot.kind === 'pdf_process') return pdfProjection(snapshot)
  if (snapshot.kind !== 'stock_adapter') throw new Error('invalid adapter projection')
  const input = row(snapshot.input)
  if (review) return transformReviewSnapshot(snapshot.operation as string, input)
  switch (snapshot.operation) {
    case 'web_filters': case 'web_request': case 'web_provider': case 'web_entries': case 'web_finalize': case 'web_extract': case 'web_images': return transformWebSnapshot(snapshot.operation, input)
    case 'github_result': return transformGithubSnapshot(input)
    case 'finance_quote': case 'finance_chart': return finance(snapshot.operation, input)
    case 'validate_data': return data(input)
    case 'speech_options': case 'speech_format': return speech(snapshot.operation, input)
    default: throw new Error('unadmitted adapter operation')
  }
}
