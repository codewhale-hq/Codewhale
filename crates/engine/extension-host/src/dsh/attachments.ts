import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { IMAGE_LIMITS } from '../protocol.generated.ts'
import { RpcError } from '../rpc.ts'
import type { AdapterHost, DshAdapter } from './adapters.ts'

const ATTACHMENT_ID = /^sha256:[0-9a-f]{64}$/
const IMAGE_INPUT_KEYS = new Set(['data', 'mediaType', 'name'])
const REF_KEYS = new Set(['attachmentId', 'mediaType', 'bytes', 'width', 'height'])
const REQUEST_TARGET_KEYS = new Set(['width', 'height', 'maxBytes'])
const MAX_NAME_LEN = 128

const FROZEN_MEDIA_TYPES = Object.freeze([...IMAGE_LIMITS.mediaTypes]) as readonly string[]
const ALLOWED_MEDIA_TYPES = new Set<string>(FROZEN_MEDIA_TYPES)

export interface ImageLimits {
  readonly maxImageBytes: number
  readonly maxImagesPerMessage: number
  readonly maxMessageImageBytes: number
  readonly maxImagePixels: number
  readonly maxImageDimension: number
  readonly mediaTypes: readonly string[]
}

export const ATTACHMENT_IMAGE_LIMITS: ImageLimits = Object.freeze({
  maxImageBytes: IMAGE_LIMITS.maxImageBytes,
  maxImagesPerMessage: IMAGE_LIMITS.maxImagesPerMessage,
  maxMessageImageBytes: IMAGE_LIMITS.maxMessageImageBytes,
  maxImagePixels: IMAGE_LIMITS.maxImagePixels,
  maxImageDimension: IMAGE_LIMITS.maxImageDimension,
  mediaTypes: FROZEN_MEDIA_TYPES,
})

export interface ImageAttachmentRef {
  attachmentId: string
  mediaType?: string
  bytes?: number
  width?: number
  height?: number
}

export interface StoredImageAttachmentRef {
  attachmentId: string
  mediaType: string
  bytes: number
  width: number
  height: number
}

export interface ImageBytesInput {
  data: Uint8Array
  mediaType: string
  name?: string
}

export interface ValidateImageResult {
  ok: true
  mediaType: string
  bytes: number
  width: number
  height: number
}

export interface ReadImageResult {
  data: Uint8Array
  ref: StoredImageAttachmentRef
}

export interface ReadImageRequestTarget {
  width?: number
  height?: number
  maxBytes?: number
}

export interface ReadImageRequestResult {
  variantId: string
  data: Uint8Array
  mediaType: string
}

export class AttachmentError extends Error {
  readonly code: string

  constructor(code: string, message: string) {
    super(message)
    this.name = 'AttachmentError'
    this.code = code
  }
}

export function isAttachmentError(error: unknown): error is AttachmentError {
  return error instanceof AttachmentError || (
    error !== null
    && typeof error === 'object'
    && (error as { name?: unknown }).name === 'AttachmentError'
    && typeof (error as { code?: unknown }).code === 'string'
  )
}

function invalid(message: string): never {
  throw new RpcError(ErrorCode.InvalidParams, message)
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value) && !ArrayBuffer.isView(value)
}

function checkAllowedKeys(value: Record<string, unknown>, allowed: Set<string>, label: string): void {
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) invalid(`${label} contains unsupported field \`${key}\``)
  }
}

function validateSignal(signal: unknown): AbortSignal | undefined {
  if (signal === undefined) return undefined
  if (!(signal instanceof AbortSignal)) {
    invalid('signal must be an AbortSignal')
  }
  return signal
}

function checkAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted === true) {
    throw new AttachmentError('CANCELLED', 'the attachment operation was aborted')
  }
}

function validateName(name: unknown): string | undefined {
  if (name === undefined) return undefined
  if (typeof name !== 'string' || name.length === 0 || name.length > MAX_NAME_LEN
    || name.startsWith('.') || name.includes('/') || name.includes('\\')
    || name.includes('..') || name.includes(':') || /[\x00-\x1f\x7f]/.test(name)) {
    invalid('attachment name must be a non-empty basename with no path separators or control characters')
  }
  return name
}

function serializeImageBytesInput(input: unknown): { data_base64: string, media_type: string, name?: string } {
  if (!isPlainRecord(input)) invalid('image input must be an object')
  checkAllowedKeys(input, IMAGE_INPUT_KEYS, 'image input')
  const { data, mediaType } = input
  if (!(data instanceof Uint8Array)) invalid('image data must be a Uint8Array')
  if (typeof mediaType !== 'string' || mediaType.length === 0) invalid('mediaType must be a non-empty string')
  if (!ALLOWED_MEDIA_TYPES.has(mediaType)) {
    throw new AttachmentError('UNSUPPORTED_IMAGE_TYPE', `unsupported image media type: ${mediaType}`)
  }
  const byteLength = data.byteLength
  if (byteLength === 0) {
    throw new AttachmentError('INVALID_IMAGE', 'image payload is empty')
  }
  if (byteLength > ATTACHMENT_IMAGE_LIMITS.maxImageBytes) {
    throw new AttachmentError('IMAGE_TOO_LARGE', `image exceeds the ${ATTACHMENT_IMAGE_LIMITS.maxImageBytes} byte limit`)
  }
  const name = validateName(input.name)
  const data_base64 = Buffer.from(data.buffer, data.byteOffset, byteLength).toString('base64')
  return name === undefined ? { data_base64, media_type: mediaType } : { data_base64, media_type: mediaType, name }
}

function positiveU32(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value <= 0 || value > 0xffff_ffff) {
    invalid(`${label} must be a positive integer`)
  }
  return value
}

function positiveSafeInt(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) {
    invalid(`${label} must be a positive integer`)
  }
  return value
}

function serializeRef(ref: unknown): Record<string, unknown> {
  if (!isPlainRecord(ref)) invalid('attachment reference must be an object')
  checkAllowedKeys(ref, REF_KEYS, 'attachment reference')
  if (typeof ref.attachmentId !== 'string' || !ATTACHMENT_ID.test(ref.attachmentId)) {
    throw new AttachmentError('INVALID_ATTACHMENT_REF', 'invalid attachment id')
  }
  const wire: Record<string, unknown> = { attachment_id: ref.attachmentId }
  if (ref.mediaType !== undefined) {
    if (typeof ref.mediaType !== 'string' || ref.mediaType.length === 0) invalid('attachment mediaType must be a string')
    wire.media_type = ref.mediaType
  }
  if (ref.bytes !== undefined) wire.bytes = positiveSafeInt(ref.bytes, 'attachment bytes')
  if (ref.width !== undefined) wire.width = positiveU32(ref.width, 'attachment width')
  if (ref.height !== undefined) wire.height = positiveU32(ref.height, 'attachment height')
  return wire
}

function serializeRequestTarget(target: unknown): Record<string, number> {
  if (target === undefined) return {}
  if (!isPlainRecord(target)) invalid('readImageRequest target must be an object')
  checkAllowedKeys(target, REQUEST_TARGET_KEYS, 'readImageRequest target')
  const wire: Record<string, number> = {}
  if (target.width !== undefined) {
    const width = positiveU32(target.width, 'width')
    if (width > ATTACHMENT_IMAGE_LIMITS.maxImageDimension) {
      throw new AttachmentError('IMAGE_DIMENSION_TOO_LARGE', `requested width exceeds the ${ATTACHMENT_IMAGE_LIMITS.maxImageDimension}px limit`)
    }
    wire.width = width
  }
  if (target.height !== undefined) {
    const height = positiveU32(target.height, 'height')
    if (height > ATTACHMENT_IMAGE_LIMITS.maxImageDimension) {
      throw new AttachmentError('IMAGE_DIMENSION_TOO_LARGE', `requested height exceeds the ${ATTACHMENT_IMAGE_LIMITS.maxImageDimension}px limit`)
    }
    wire.height = height
  }
  if (target.maxBytes !== undefined) {
    const maxBytes = positiveSafeInt(target.maxBytes, 'maxBytes')
    if (maxBytes > ATTACHMENT_IMAGE_LIMITS.maxImageBytes) {
      throw new AttachmentError('IMAGE_TOO_LARGE', `requested maxBytes exceeds the ${ATTACHMENT_IMAGE_LIMITS.maxImageBytes} byte limit`)
    }
    wire.max_bytes = maxBytes
  }
  return wire
}

function parseRefWire(value: unknown, what: string): StoredImageAttachmentRef {
  if (!isPlainRecord(value)
    || typeof value.attachment_id !== 'string' || !ATTACHMENT_ID.test(value.attachment_id)
    || typeof value.media_type !== 'string' || !ALLOWED_MEDIA_TYPES.has(value.media_type)
    || typeof value.bytes !== 'number' || !Number.isSafeInteger(value.bytes) || value.bytes <= 0
    || typeof value.width !== 'number' || !Number.isInteger(value.width) || value.width <= 0
    || typeof value.height !== 'number' || !Number.isInteger(value.height) || value.height <= 0) {
    throw new RpcError(ErrorCode.Internal, `invalid ${what}`)
  }
  return {
    attachmentId: value.attachment_id,
    mediaType: value.media_type,
    bytes: value.bytes,
    width: value.width,
    height: value.height,
  }
}

function mapBrokerError(error: unknown): unknown {
  if (error instanceof Error && error.name === 'AbortError') {
    return new AttachmentError('CANCELLED', error.message || 'the attachment operation was aborted')
  }
  if (error !== null && typeof error === 'object') {
    const code = (error as { code?: unknown }).code
    const message = (error as { message?: unknown }).message
    if (code === ErrorCode.Cancelled) {
      return new AttachmentError('CANCELLED', typeof message === 'string' && message.length > 0 ? message : 'the attachments broker call was cancelled')
    }
    if (code === ErrorCode.Refused && typeof message === 'string') {
      const match = /^\[([A-Z0-9_]+)\]\s*(.+)$/s.exec(message)
      if (match !== null) {
        return new AttachmentError(match[1], match[2])
      }
    }
  }
  return error
}

async function callBroker(
  host: AdapterHost,
  ctx: unknown,
  method: string,
  input: unknown,
  signal?: AbortSignal,
): Promise<unknown> {
  const checkedSignal = validateSignal(signal)
  checkAborted(checkedSignal)
  const settled = host.broker(ctx, 'attachments', method, input, checkedSignal).catch((error) => {
    throw mapBrokerError(error)
  })
  settled.catch(() => undefined)
  if (checkedSignal === undefined) {
    return await settled
  }
  let onAbort: (() => void) | undefined
  try {
    const aborted = new Promise<never>((_, reject) => {
      onAbort = () => reject(new AttachmentError('CANCELLED', 'the attachment operation was aborted'))
      checkedSignal.addEventListener('abort', onAbort, { once: true })
    })
    const result = await Promise.race([settled, aborted])
    checkAborted(checkedSignal)
    return result
  } finally {
    if (onAbort !== undefined) checkedSignal.removeEventListener('abort', onAbort)
  }
}

export const attachmentsAdapter: DshAdapter = {
  service: 'attachments',
  define(host: AdapterHost) {
    class AttachmentsService extends Service {
      constructor(ctx: any) { super(ctx, 'attachments') }

      get imageLimits(): ImageLimits {
        return ATTACHMENT_IMAGE_LIMITS
      }

      imageHostPath(_ref?: unknown, signal?: AbortSignal): undefined {
        const checkedSignal = validateSignal(signal)
        checkAborted(checkedSignal)
        return undefined
      }

      async validateImage(input: ImageBytesInput, signal?: AbortSignal): Promise<ValidateImageResult> {
        const wire = serializeImageBytesInput(input)
        const res = await callBroker(host, this.ctx, 'validate', wire, signal)
        if (!isPlainRecord(res)
          || res.ok !== true
          || typeof res.media_type !== 'string' || !ALLOWED_MEDIA_TYPES.has(res.media_type)
          || typeof res.bytes !== 'number' || !Number.isSafeInteger(res.bytes) || res.bytes <= 0
          || typeof res.width !== 'number' || !Number.isInteger(res.width) || res.width <= 0
          || typeof res.height !== 'number' || !Number.isInteger(res.height) || res.height <= 0) {
          throw new RpcError(ErrorCode.Internal, 'invalid attachments.validate broker response')
        }
        return {
          ok: true,
          mediaType: res.media_type,
          bytes: res.bytes,
          width: res.width,
          height: res.height,
        }
      }

      async saveImage(input: ImageBytesInput, signal?: AbortSignal): Promise<StoredImageAttachmentRef> {
        const wire = serializeImageBytesInput(input)
        const res = await callBroker(host, this.ctx, 'save', wire, signal)
        if (!isPlainRecord(res)) throw new RpcError(ErrorCode.Internal, 'invalid attachments.save broker response')
        return parseRefWire(res.attachment, 'attachments.save attachment reference')
      }

      async readImage(ref: ImageAttachmentRef, signal?: AbortSignal): Promise<ReadImageResult> {
        const attachment = serializeRef(ref)
        const res = await callBroker(host, this.ctx, 'read', { attachment }, signal)
        if (!isPlainRecord(res) || typeof res.data_base64 !== 'string') {
          throw new RpcError(ErrorCode.Internal, 'invalid attachments.read broker response')
        }
        return {
          data: Buffer.from(res.data_base64, 'base64'),
          ref: parseRefWire(res.attachment, 'attachments.read attachment reference'),
        }
      }

      async readImageRequest(
        ref: ImageAttachmentRef,
        target?: ReadImageRequestTarget,
        signal?: AbortSignal,
      ): Promise<ReadImageRequestResult> {
        const attachment = serializeRef(ref)
        const sizing = serializeRequestTarget(target)
        const res = await callBroker(host, this.ctx, 'read_request', { attachment, ...sizing }, signal)
        if (!isPlainRecord(res)
          || typeof res.variant_id !== 'string' || !ATTACHMENT_ID.test(res.variant_id)
          || typeof res.data_base64 !== 'string'
          || typeof res.media_type !== 'string' || !ALLOWED_MEDIA_TYPES.has(res.media_type)) {
          throw new RpcError(ErrorCode.Internal, 'invalid attachments.read_request broker response')
        }
        return {
          variantId: res.variant_id,
          data: Buffer.from(res.data_base64, 'base64'),
          mediaType: res.media_type,
        }
      }

      saveFile(): never { return notAvailable('saveFile') }
      admitEncodedFile(): never { return notAvailable('admitEncodedFile') }
      saveFileStream(): never { return notAvailable('saveFileStream') }
      readFileStream(): never { return notAvailable('readFileStream') }
      readFile(): never { return notAvailable('readFile') }
      fileHostPath(): never { return notAvailable('fileHostPath') }
    }
    Object.freeze(AttachmentsService.prototype)
    return AttachmentsService
  },
}

function notAvailable(method: string): never {
  throw new RpcError(
    ErrorCode.NotAvailable,
    `attachments.${method} is not available: Codewhale brokers conversation images only and exposes no generic file store`,
  )
}
