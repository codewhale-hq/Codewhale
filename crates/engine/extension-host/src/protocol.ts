/**
 * Codewhale extension-host protocol, version 1.
 *
 * The Rust serde types in `crates/engine/src/extension_host/protocol.rs` are the
 * source of truth. The constants, the method table, every params shape and the
 * wire types come from `protocol.generated.ts`, which a Rust test renders from
 * those types and fails on when the committed file drifts. Hand-written here:
 * the frame codec, the JSON-RPC envelope checks, and the one rule Rust applies
 * beyond its types (`host/hello`'s runtime name). Both sides also parse the
 * shared corpus in `crates/engine/tests/fixtures/extension_host/protocol`.
 *
 * Frame: 4-byte magic `CWX1`, u32 little-endian payload length, UTF-8 JSON.
 * Envelope: JSON-RPC 2.0.
 */
import { MAGIC_ASCII, MAX_FRAME, HEADER_LEN, METHODS, SHAPES } from './protocol.generated.ts'
import type { Direction, HostTier, Kind, RpcErrorWire, Shape } from './protocol.generated.ts'

export * from './protocol.generated.ts'

export const MAGIC = Buffer.from(MAGIC_ASCII, 'ascii')

/** One row of the method table (`METHODS`). */
export interface MethodRow {
  readonly name: string
  readonly direction: Direction
  readonly request: boolean
  readonly params: string
  readonly tiers: readonly HostTier[]
}

export type Message =
  | { jsonrpc: '2.0'; id: number; method: string; params?: any }
  | { jsonrpc: '2.0'; method: string; params?: any }
  | { jsonrpc: '2.0'; id: number; result: any }
  | { jsonrpc: '2.0'; id: number; error: RpcErrorWire }

export class FrameError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'FrameError'
  }
}

export class ProtocolError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'ProtocolError'
  }
}

/** Encode one message into a `CWX1` frame. Oversized payloads are refused, never truncated. */
export function encodeFrame(message: unknown): Buffer {
  const payload = Buffer.from(JSON.stringify(message), 'utf8')
  if (payload.length > MAX_FRAME) {
    throw new FrameError(`frame of ${payload.length} bytes exceeds MAX_FRAME ${MAX_FRAME}`)
  }
  const header = Buffer.alloc(HEADER_LEN)
  MAGIC.copy(header, 0)
  header.writeUInt32LE(payload.length, 4)
  return Buffer.concat([header, payload])
}

/** Incremental `CWX1` decoder. Throws `FrameError` on bad magic, bad length, or bad JSON. */
export class FrameDecoder {
  private buffer: Buffer = Buffer.alloc(0)

  push(chunk: Buffer): unknown[] {
    this.buffer = this.buffer.length === 0 ? chunk : Buffer.concat([this.buffer, chunk])
    const out: unknown[] = []
    while (this.buffer.length >= HEADER_LEN) {
      if (!this.buffer.subarray(0, 4).equals(MAGIC)) {
        throw new FrameError('bad frame magic')
      }
      const length = this.buffer.readUInt32LE(4)
      if (length > MAX_FRAME) throw new FrameError(`frame length ${length} exceeds MAX_FRAME`)
      if (this.buffer.length < HEADER_LEN + length) break
      const payload = this.buffer.subarray(HEADER_LEN, HEADER_LEN + length)
      this.buffer = this.buffer.subarray(HEADER_LEN + length)
      let value: unknown
      try {
        value = JSON.parse(payload.toString('utf8'))
      } catch (error) {
        throw new FrameError(`frame payload is not JSON: ${(error as Error).message}`)
      }
      out.push(value)
    }
    return out
  }
}

// ---------------------------------------------------------------------------
// Validation, driven by the generated shapes. A shape is strict (unknown fields
// rejected) exactly where the Rust type has `deny_unknown_fields`: every
// host→core type and `OwnerRef`. The envelope is checked strictly for
// host→core messages and tolerantly for core→host ones.
// ---------------------------------------------------------------------------

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function checkShape(where: string, value: unknown, shape: Shape): asserts value is Record<string, any> {
  if (!isObject(value)) throw new ProtocolError(`${where}: expected an object`)
  for (const [key, kind] of Object.entries(shape.required)) {
    if (!(key in value)) throw new ProtocolError(`${where}: missing field \`${key}\``)
    checkKind(`${where}.${key}`, value[key], kind)
  }
  for (const [key, kind] of Object.entries(shape.optional)) {
    // Every optional Rust field is an `Option` or a defaulted JSON value, so
    // an explicit `null` reads as absent there too.
    if (value[key] !== undefined && value[key] !== null) checkKind(`${where}.${key}`, value[key], kind)
  }
  if (shape.strict) {
    for (const key of Object.keys(value)) {
      if (!(key in shape.required) && !(key in shape.optional)) {
        throw new ProtocolError(`${where}: unknown field \`${key}\``)
      }
    }
  }
}

function checkKind(where: string, value: unknown, kind: Kind): void {
  if (typeof kind === 'object') {
    if ('ref' in kind) return checkShape(where, value, SHAPES[kind.ref])
    if ('enum' in kind) {
      if (typeof value !== 'string' || !kind.enum.includes(value)) {
        throw new ProtocolError(`${where}: expected one of ${kind.enum.map((v) => `\`${v}\``).join(', ')}`)
      }
      return
    }
    if (!Array.isArray(value)) throw new ProtocolError(`${where}: expected an array`)
    value.forEach((item, index) => checkKind(`${where}[${index}]`, item, kind.items))
    return
  }
  switch (kind) {
    case 'string':
      if (typeof value !== 'string') throw new ProtocolError(`${where}: expected a string`)
      return
    case 'uint':
      if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
        throw new ProtocolError(`${where}: expected an unsigned integer`)
      }
      return
    case 'integer':
      if (typeof value !== 'number' || !Number.isSafeInteger(value)) throw new ProtocolError(`${where}: expected an integer`)
      return
    case 'boolean':
      if (typeof value !== 'boolean') throw new ProtocolError(`${where}: expected a boolean`)
      return
    case 'object':
      if (!isObject(value)) throw new ProtocolError(`${where}: expected an object`)
      return
    case 'json':
      return
  }
}

/**
 * Validate one decoded message travelling in `direction` to or from a host of
 * `tier`. Only methods in the generated table are admitted, each with its
 * params shape and only on the tiers its row allows (a method reserved for the
 * built-in tier is neither sent nor accepted by a plugin-tier host). Responses
 * are validated as envelopes only: their result shape depends on the request,
 * which the RPC layer checks. `methods` is the table to admit from; only a test
 * of the tier rule passes anything but the generated one.
 */
export function validateMessage(
  value: unknown,
  direction: Direction,
  tier: HostTier,
  methods: readonly MethodRow[] = METHODS,
): Message {
  const strict = direction === 'host_to_core'
  if (!isObject(value)) throw new ProtocolError('message: expected an object')
  if (value.jsonrpc !== '2.0') throw new ProtocolError('message: jsonrpc must be "2.0"')
  const hasId = 'id' in value
  if (hasId) checkKind('message.id', value.id, 'uint')
  if ('method' in value) {
    checkShape('message', value, { strict, required: { jsonrpc: 'string', method: 'string' }, optional: { id: 'uint', params: 'json' } })
    const method = value.method as string
    const spec = methods.find((entry) => entry.name === method && entry.direction === direction)
    if (!spec) throw new ProtocolError(`unknown ${direction} method \`${method}\``)
    if (!spec.tiers.includes(tier)) throw new ProtocolError(`\`${method}\` is not allowed on the ${tier} tier`)
    if (spec.request !== hasId) {
      throw new ProtocolError(`\`${method}\` must be ${spec.request ? 'a request (with id)' : 'a notification (no id)'}`)
    }
    const params = 'params' in value ? value.params : {}
    checkShape(method, params, SHAPES[spec.params])
    // Rust checks this after decoding `HelloParams` (`parse_host_message`).
    if (method === 'host/hello' && params.runtime.name !== 'bun' && params.runtime.name !== 'node') {
      throw new ProtocolError(`host/hello.runtime.name: unknown runtime \`${params.runtime.name}\``)
    }
    // Which spec fields each kind uses: `RegisterParams::check_spec`.
    if (method === 'registry/register') {
      const { kind, spec } = params
      const reason =
        kind === 'tool' && spec.input_schema == null
          ? 'a tool registration needs `spec.input_schema`'
          : kind === 'tool' && spec.argument_hint != null
            ? 'a tool registration has no `spec.argument_hint`'
            : kind === 'command' && spec.input_schema != null
              ? 'a command registration has no `spec.input_schema`'
              : (kind === 'hook' || kind === 'prompt_section' || kind === 'prompt_template' || kind === 'skill_root' || kind === 'avatar_pack' || kind === 'shell_hook' || kind === 'mcp_server') && (spec.input_schema != null || spec.argument_hint != null)
                ? 'a hook, prompt or skill root registration has no input schema or argument hint'
              : undefined
      if (reason !== undefined) throw new ProtocolError(`${method}: ${reason}`)
    }
    return value as Message
  }
  if (!hasId) throw new ProtocolError('response: missing id')
  if ('error' in value) {
    checkShape('message', value, { strict, required: { jsonrpc: 'string', id: 'uint', error: { ref: 'RpcErrorWire' } }, optional: {} })
    return value as Message
  }
  if (!('result' in value)) throw new ProtocolError('response: needs result or error')
  checkShape('message', value, { strict, required: { jsonrpc: 'string', id: 'uint', result: 'json' }, optional: {} })
  return value as Message
}
