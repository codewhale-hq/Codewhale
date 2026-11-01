// @generated from the Rust protocol types in crates/engine/src/extension_host/protocol.rs
// by `extension_host::protocol::tests`. Do not edit: change the Rust side, re-record with
//   CODEWHALE_CONFORMANCE_UPDATE=1 cargo test -p codewhale-tui --lib extension_host::protocol
// and rebuild dist/ with `npm run build`.

export const PROTOCOL_VERSION = 1
export const MAGIC_ASCII = 'CWX1'
export const HEADER_LEN = 8
export const MAX_FRAME = 33554432
export const MAX_INFLIGHT = 256

/** JSON-RPC error codes used on this channel. */
export const ErrorCode = {
  ParseError: -32700,
  InvalidRequest: -32600,
  MethodNotFound: -32601,
  InvalidParams: -32602,
  Internal: -32603,
  ExecutionFailed: -32000,
  NotAvailable: -32001,
  Refused: -32002,
  Denied: -32003,
  Cancelled: -32800,
} as const

export type Direction = 'core_to_host' | 'host_to_core'

/** Every method either side may send, and the trust tiers it is allowed on; nothing else is admitted. */
export const METHODS = [
  { name: 'host/initialize', direction: 'core_to_host', request: true, params: 'InitializeParams', tiers: ['plugin', 'builtin'] },
  { name: 'host/ping', direction: 'core_to_host', request: true, params: 'EmptyParams', tiers: ['plugin', 'builtin'] },
  { name: 'host/shutdown', direction: 'core_to_host', request: true, params: 'EmptyParams', tiers: ['plugin', 'builtin'] },
  { name: 'ext/activate', direction: 'core_to_host', request: true, params: 'ActivateParams', tiers: ['plugin', 'builtin'] },
  { name: 'ext/deactivate', direction: 'core_to_host', request: true, params: 'DeactivateParams', tiers: ['plugin', 'builtin'] },
  { name: 'tool/call', direction: 'core_to_host', request: true, params: 'ToolCallParams', tiers: ['plugin', 'builtin'] },
  { name: 'command/run', direction: 'core_to_host', request: true, params: 'CommandRunParams', tiers: ['plugin', 'builtin'] },
  { name: 'hook/evaluate', direction: 'core_to_host', request: true, params: 'HookEvaluateParams', tiers: ['plugin', 'builtin'] },
  { name: 'mcp/open', direction: 'core_to_host', request: true, params: 'McpOpenParams', tiers: ['builtin'] },
  { name: 'mcp/request', direction: 'core_to_host', request: true, params: 'McpRequestParams', tiers: ['builtin'] },
  { name: 'mcp/close', direction: 'core_to_host', request: true, params: 'McpCloseParams', tiers: ['builtin'] },
  { name: 'harness/run', direction: 'core_to_host', request: true, params: 'HarnessRunParams', tiers: ['builtin'] },
  { name: '$/cancel', direction: 'core_to_host', request: false, params: 'CancelParams', tiers: ['plugin', 'builtin'] },
  { name: 'host/hello', direction: 'host_to_core', request: false, params: 'HelloParams', tiers: ['plugin', 'builtin'] },
  { name: 'host/ready', direction: 'host_to_core', request: false, params: 'EmptyParams', tiers: ['plugin', 'builtin'] },
  { name: 'registry/register', direction: 'host_to_core', request: true, params: 'RegisterParams', tiers: ['plugin', 'builtin'] },
  { name: 'registry/unregister', direction: 'host_to_core', request: true, params: 'UnregisterParams', tiers: ['plugin', 'builtin'] },
  { name: 'core/call', direction: 'host_to_core', request: true, params: 'CoreCallParams', tiers: ['plugin', 'builtin'] },
  { name: 'session/checkpoint', direction: 'host_to_core', request: true, params: 'SessionCheckpointParams', tiers: ['plugin', 'builtin'] },
  { name: 'user-questions/ask', direction: 'host_to_core', request: true, params: 'UserQuestionParams', tiers: ['plugin', 'builtin'] },
  { name: 'subagent/start', direction: 'host_to_core', request: true, params: 'SubagentStartParams', tiers: ['plugin', 'builtin'] },
  { name: 'subagent/result', direction: 'host_to_core', request: true, params: 'SubagentHandleParams', tiers: ['plugin', 'builtin'] },
  { name: 'subagent/dispose', direction: 'host_to_core', request: true, params: 'SubagentHandleParams', tiers: ['plugin', 'builtin'] },
  { name: 'proc/launch', direction: 'host_to_core', request: true, params: 'ProcLaunchParams', tiers: ['builtin'] },
  { name: 'proc/read', direction: 'host_to_core', request: true, params: 'ProcSessionParams', tiers: ['builtin'] },
  { name: 'proc/write', direction: 'host_to_core', request: true, params: 'ProcWriteParams', tiers: ['builtin'] },
  { name: 'proc/close', direction: 'host_to_core', request: true, params: 'ProcSessionParams', tiers: ['builtin'] },
  { name: 'net/start', direction: 'host_to_core', request: true, params: 'ProcLaunchParams', tiers: ['builtin'] },
  { name: 'net/fetch', direction: 'host_to_core', request: true, params: 'NetFetchParams', tiers: ['builtin'] },
  { name: 'net/read', direction: 'host_to_core', request: true, params: 'NetReadParams', tiers: ['builtin'] },
  { name: 'net/release', direction: 'host_to_core', request: true, params: 'NetReadParams', tiers: ['builtin'] },
  { name: 'net/close', direction: 'host_to_core', request: true, params: 'ProcSessionParams', tiers: ['builtin'] },
  { name: 'exec/redeem', direction: 'host_to_core', request: true, params: 'ExecutionRedeemParams', tiers: ['builtin'] },
  { name: 'ext/faulted', direction: 'host_to_core', request: false, params: 'FaultedParams', tiers: ['plugin', 'builtin'] },
  { name: 'log', direction: 'host_to_core', request: false, params: 'LogParams', tiers: ['plugin', 'builtin'] },
  { name: '$/cancel', direction: 'host_to_core', request: false, params: 'CancelParams', tiers: ['plugin', 'builtin'] },
] as const

/** A field's wire kind: the Rust field's serde type, normalized for validation. */
export type Kind =
  | 'string'
  | 'boolean'
  | 'uint'
  | 'integer'
  | 'object'
  | 'json'
  | { readonly ref: string }
  | { readonly enum: readonly string[] }
  | { readonly items: Kind }

/** An object's fields; `strict` is Rust's `deny_unknown_fields`. */
export interface Shape {
  readonly strict: boolean
  readonly required: { readonly [field: string]: Kind }
  readonly optional: { readonly [field: string]: Kind }
}

/** Every method's params, and an error response's `error`. */
export const SHAPES: { readonly [name: string]: Shape } = {
  ActivateParams: {
    strict: false,
    required: { owner: { ref: 'OwnerRef' }, plugin_name: 'string', entry: { ref: 'EntryRef' } },
    optional: { scope: { ref: 'EntryRef' }, config: 'json', data_dir: 'string' },
  },
  CancelParams: {
    strict: true,
    required: { id: 'uint' },
    optional: {},
  },
  CommandRunParams: {
    strict: false,
    required: { handle: 'uint', command_id: 'string', raw_input: 'string', deadline_ms: 'uint' },
    optional: { workspace: 'string', session_id: 'string', agent_id: 'string', origin_turn_id: 'string' },
  },
  CoreCallParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, ticket: 'string', name: 'string', input: 'json' },
    optional: {},
  },
  DeactivateParams: {
    strict: false,
    required: { owner: { ref: 'OwnerRef' } },
    optional: { entry: { ref: 'EntryRef' } },
  },
  EmptyParams: {
    strict: true,
    required: {},
    optional: {},
  },
  EntryRef: {
    strict: false,
    required: { path: 'string', sha256: 'string' },
    optional: {},
  },
  ExecutionRedeemParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, execution_id: 'string', ticket: 'string' },
    optional: {},
  },
  FaultedParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, error: 'string' },
    optional: {},
  },
  HarnessRunParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, execution_id: 'string', ticket: 'string', deadline_ms: 'uint' },
    optional: { hook: { ref: 'HookDispatchWire' } },
  },
  HelloParams: {
    strict: true,
    required: { protocol: { ref: 'ProtocolRange' }, host_version: 'string', bundle_sha256: 'string', runtime: { ref: 'HelloRuntime' }, tier: { enum: ['builtin', 'plugin'] }, builtin_modules: { items: { ref: 'ModuleDigestWire' } } },
    optional: { memory_limit_mib: 'uint' },
  },
  HelloRuntime: {
    strict: true,
    required: { name: 'string', version: 'string' },
    optional: {},
  },
  HookCallPayload: {
    strict: true,
    required: { name: 'string', call_id: 'string', input: 'json', mode: 'string', workspace: 'string', model: 'string' },
    optional: {},
  },
  HookDispatchWire: {
    strict: true,
    required: { event: 'string', dialect: 'string', point: 'string', query: 'string' },
    optional: { matcher: 'string' },
  },
  HookEvaluateParams: {
    strict: true,
    required: { handle: 'uint', event: 'string', payload: { ref: 'HookCallPayload' }, deadline_ms: 'uint' },
    optional: {},
  },
  HostLimits: {
    strict: false,
    required: { max_frame: 'uint', max_inflight: 'uint', dispose_deadline_ms: 'uint', activate_deadline_ms: 'uint' },
    optional: {},
  },
  InitializeParams: {
    strict: false,
    required: { protocol: 'uint', limits: { ref: 'HostLimits' } },
    optional: {},
  },
  LogParams: {
    strict: true,
    required: { level: 'string', msg: 'string' },
    optional: { plugin_id: 'string' },
  },
  McpCloseParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, session_id: 'string', deadline_ms: 'uint' },
    optional: {},
  },
  McpHttpHeaders: {
    strict: true,
    required: {},
    optional: { accept: 'string', content_type: 'string', mcp_session_id: 'string', mcp_protocol_version: 'string', mcp_method: 'string', mcp_name: 'string' },
  },
  McpOpenParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, session_id: 'string', launch_ticket: 'string', transport: 'string', initialize_grant: { ref: 'McpOperationGrant' }, initialized_grant: { ref: 'McpOperationGrant' }, client_version: 'string', deadline_ms: 'uint' },
    optional: { discover_grant: { ref: 'McpOperationGrant' } },
  },
  McpOperationGrant: {
    strict: true,
    required: { ticket: 'string', operation_id: 'string', method: 'string', params: 'json' },
    optional: { wire_id: 'string' },
  },
  McpRequestParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, session_id: 'string', grant: { ref: 'McpOperationGrant' }, deadline_ms: 'uint' },
    optional: {},
  },
  ModuleDigestWire: {
    strict: true,
    required: { id: 'string', sha256: 'string' },
    optional: {},
  },
  NetFetchParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, session_id: 'string', url: 'string', method: 'string', headers: { ref: 'McpHttpHeaders' } },
    optional: { frame: 'json', ticket: 'string', operation_id: 'string' },
  },
  NetReadParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, session_id: 'string', response_id: 'string' },
    optional: {},
  },
  OwnerRef: {
    strict: true,
    required: { plugin_id: 'string', generation: 'uint', owner_token: 'string' },
    optional: {},
  },
  ProcLaunchParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, session_id: 'string', ticket: 'string' },
    optional: {},
  },
  ProcSessionParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, session_id: 'string' },
    optional: {},
  },
  ProcWriteParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, session_id: 'string', frame: 'json' },
    optional: { ticket: 'string', operation_id: 'string' },
  },
  ProtocolRange: {
    strict: true,
    required: { min: 'uint', max: 'uint' },
    optional: {},
  },
  RegisterParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, kind: { enum: ['tool', 'command', 'hook', 'prompt_section', 'prompt_template', 'skill_root', 'avatar_pack', 'shell_hook', 'mcp_server'] }, spec: { ref: 'RegisterSpecWire' } },
    optional: { scope: { ref: 'EntryRef' } },
  },
  RegisterSpecWire: {
    strict: true,
    required: { name: 'string', description: 'string' },
    optional: { input_schema: 'object', argument_hint: 'string' },
  },
  RpcErrorWire: {
    strict: true,
    required: { code: 'integer', message: 'string' },
    optional: { data: 'json' },
  },
  SessionCheckpointParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, ticket: 'string' },
    optional: {},
  },
  SubagentHandleParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, ticket: 'string', child_id: 'string' },
    optional: {},
  },
  SubagentStartParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, ticket: 'string', child_id: 'string', prompt: 'string', delivery: 'string' },
    optional: { model: 'string', persona: 'string', denied_tools: { items: 'string' } },
  },
  ToolCallParams: {
    strict: false,
    required: { handle: 'uint', call_id: 'string', input: 'json', deadline_ms: 'uint' },
    optional: { workspace: 'string', ticket: 'string', checkpoint_ticket: 'string', question_ticket: 'string', subagent_ticket: 'string', caller_tools: { items: 'string' }, session_id: 'string', agent_id: 'string', origin_turn_id: 'string' },
  },
  UnregisterParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, handle: 'uint' },
    optional: {},
  },
  UserInputIntent: {
    strict: false,
    required: { approve: 'string', kind: { enum: ['plan-review'] } },
    optional: { call_id: 'string' },
  },
  UserInputOption: {
    strict: false,
    required: { label: 'string' },
    optional: { description: 'string' },
  },
  UserInputQuestion: {
    strict: false,
    required: { id: 'string', question: 'string' },
    optional: { header: 'string', detail: 'string', intent: { ref: 'UserInputIntent' }, options: { items: { ref: 'UserInputOption' } }, allow_free_text: 'boolean', multi_select: 'boolean' },
  },
  UserQuestionParams: {
    strict: true,
    required: { owner: { ref: 'OwnerRef' }, ticket: 'string', questions: { items: { ref: 'UserInputQuestion' } } },
    optional: {},
  },
}

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json }

export interface ActivateParams {
  owner: OwnerRef
  scope?: EntryRef
  plugin_name: string
  entry: EntryRef
  config?: Json
  data_dir?: string
}

export type ActivateResult = { status: 'ok'; tools: string[]; commands?: string[] } | { status: 'failed'; diagnostic: string }

export interface CancelParams {
  id: number
}

export type CommandResultWire = { kind: 'success'; text?: string } | { kind: 'error'; text: string } | { kind: 'submit'; prompt: string; text?: string }

export interface CommandRunParams {
  handle: number
  command_id: string
  raw_input: string
  deadline_ms: number
  workspace?: string
  session_id?: string
  agent_id?: string
  origin_turn_id?: string
}

export interface ContentBlockWire {
  text: string
  type: 'text'
}

export interface CoreCallParams {
  owner: OwnerRef
  ticket: string
  name: string
  input: Json
}

export interface DeactivateParams {
  owner: OwnerRef
  entry?: EntryRef
}

export interface DeactivateResult {
  disposed: boolean
  leaked: string[]
}

export interface EmptyParams {}

export interface EntryRef {
  path: string
  sha256: string
}

export interface ExecutionRedeemParams {
  owner: OwnerRef
  execution_id: string
  ticket: string
}

export interface FaultedParams {
  owner: OwnerRef
  error: string
}

export interface HarnessRunParams {
  owner: OwnerRef
  execution_id: string
  ticket: string
  deadline_ms: number
  hook?: HookDispatchWire
}

export interface HelloParams {
  protocol: ProtocolRange
  host_version: string
  bundle_sha256: string
  runtime: HelloRuntime
  tier: HostTier
  builtin_modules: ModuleDigestWire[]
  memory_limit_mib?: number
}

export interface HelloRuntime {
  name: string
  version: string
}

export interface HookCallPayload {
  name: string
  call_id: string
  input: Json
  mode: string
  workspace: string
  model: string
}

export interface HookDispatchWire {
  event: string
  dialect: string
  point: string
  matcher?: string
  query: string
}

export interface HookEvaluateParams {
  handle: number
  event: string
  payload: HookCallPayload
  deadline_ms: number
}

export type HookVerdictWire = { kind: 'abstain' } | { kind: 'deny'; reason: string } | { kind: 'ask'; reason: string } | { kind: 'annotate'; text: string } | { kind: 'revise'; input: { [key: string]: Json } }

export interface HostLimits {
  max_frame: number
  max_inflight: number
  dispose_deadline_ms: number
  activate_deadline_ms: number
}

export type HostTier = 'builtin' | 'plugin'

export interface InitializeParams {
  protocol: number
  limits: HostLimits
}

export interface LogParams {
  level: string
  msg: string
  plugin_id?: string
}

export interface McpCloseParams {
  owner: OwnerRef
  session_id: string
  deadline_ms: number
}

export interface McpHttpHeaders {
  accept?: string
  content_type?: string
  mcp_session_id?: string
  mcp_protocol_version?: string
  mcp_method?: string
  mcp_name?: string
}

export interface McpOpenParams {
  owner: OwnerRef
  session_id: string
  launch_ticket: string
  transport: string
  discover_grant?: McpOperationGrant
  initialize_grant: McpOperationGrant
  initialized_grant: McpOperationGrant
  client_version: string
  deadline_ms: number
}

export interface McpOperationGrant {
  ticket: string
  operation_id: string
  method: string
  wire_id?: string
  params: Json
}

export interface McpRequestParams {
  owner: OwnerRef
  session_id: string
  grant: McpOperationGrant
  deadline_ms: number
}

export interface ModuleDigestWire {
  id: string
  sha256: string
}

export interface NetFetchParams {
  owner: OwnerRef
  session_id: string
  url: string
  method: string
  headers: McpHttpHeaders
  frame?: Json
  ticket?: string
  operation_id?: string
}

export interface NetReadParams {
  owner: OwnerRef
  session_id: string
  response_id: string
}

export interface OwnerRef {
  plugin_id: string
  generation: number
  owner_token: string
}

export interface ProcLaunchParams {
  owner: OwnerRef
  session_id: string
  ticket: string
}

export interface ProcSessionParams {
  owner: OwnerRef
  session_id: string
}

export interface ProcWriteParams {
  owner: OwnerRef
  session_id: string
  frame: Json
  ticket?: string
  operation_id?: string
}

export interface ProtocolRange {
  min: number
  max: number
}

export type RegisterKind = 'tool' | 'command' | 'hook' | 'prompt_section' | 'prompt_template' | 'skill_root' | 'avatar_pack' | 'shell_hook' | 'mcp_server'

export interface RegisterParams {
  owner: OwnerRef
  scope?: EntryRef
  kind: RegisterKind
  spec: RegisterSpecWire
}

export type RegisterResult = { handle: number } | { refused: string }

export interface RegisterSpecWire {
  name: string
  description: string
  input_schema?: { [key: string]: Json }
  argument_hint?: string
}

export interface RpcErrorWire {
  code: number
  message: string
  data?: Json
}

export interface SessionCheckpointParams {
  owner: OwnerRef
  ticket: string
}

export interface SubagentHandleParams {
  owner: OwnerRef
  ticket: string
  child_id: string
}

export interface SubagentStartParams {
  owner: OwnerRef
  ticket: string
  child_id: string
  prompt: string
  model?: string
  persona?: string
  denied_tools?: string[]
  delivery: string
}

export interface ToolCallParams {
  handle: number
  call_id: string
  input: Json
  deadline_ms: number
  workspace?: string
  ticket?: string
  checkpoint_ticket?: string
  question_ticket?: string
  subagent_ticket?: string
  caller_tools?: string[]
  session_id?: string
  agent_id?: string
  origin_turn_id?: string
}

export interface ToolResultWire {
  content: ContentBlockWire[]
  is_error: boolean
  structured?: Json
}

export interface UnregisterParams {
  owner: OwnerRef
  handle: number
}

export interface UserInputAnswer {
  kind?: UserInputAnswerKind
  id: string
  label: string
  value: string
}

export type UserInputAnswerKind = 'choice' | 'custom' | 'skipped'

export interface UserInputIntent {
  approve: string
  call_id?: string
  kind: 'plan-review'
}

export interface UserInputOption {
  label: string
  description?: string
}

export interface UserInputQuestion {
  header?: string
  id: string
  question: string
  detail?: string
  intent?: UserInputIntent
  options?: UserInputOption[]
  allow_free_text?: boolean
  multi_select?: boolean
}

export interface UserInputResponse {
  answers: UserInputAnswer[]
}

export interface UserQuestionParams {
  owner: OwnerRef
  ticket: string
  questions: UserInputQuestion[]
}
