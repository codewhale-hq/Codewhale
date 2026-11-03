
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json | undefined }

export type DynamicToolCallContent = { type: 'input_text'; text: string } | { type: 'input_image'; image_url: string }

export type DynamicToolCallParams = {
  thread_id: string
  turn_id: string
  call_id: string
  namespace?: string
  tool: string
  arguments: Json
  [key: string]: Json | undefined
}

export type DynamicToolCallResult = {
  success: boolean
  content?: DynamicToolCallContent[]
  [key: string]: Json | undefined
}

export type DynamicToolItemStatus = 'in_progress' | 'completed' | 'failed'

export type DynamicToolSpec = {
  namespace?: (string | null)
  name: string
  description: string
  input_schema: Json
  defer_loading?: boolean
  [key: string]: Json | undefined
}

export type DynamicToolSpecOutput = {
  namespace?: string
  name: string
  description: string
  input_schema: Json
  defer_loading: boolean
  [key: string]: Json | undefined
}

export type RuntimeCapabilities = {
  client_token_intents: boolean
  account_session: boolean
  account_model_owner: boolean
  session_provider_keys: boolean
  threads: boolean
  thread_create_idempotency: boolean
  thread_idle_precondition: boolean
  thread_shell_consent: boolean
  turns: boolean
  turn_operation_idempotency: boolean
  turn_operation_lookup: boolean
  watch_event_turns: boolean
  structured_review_turns: boolean
  structured_review_version: number
  automation_mutation_preconditions: boolean
  turn_image_inputs: boolean
  turn_output_token_limit: boolean
  profile_constitution: boolean
  turn_steer: boolean
  turn_interrupt: boolean
  event_replay: boolean
  external_tools: boolean
  environments: boolean
  worker_runtime: boolean
  fleet_run_create: boolean
  thread_fleet_members: boolean
  fleet_run_start: boolean
  fleet_event_replay: boolean
  fleet_event_stream: boolean
  fleet_local_target: boolean
  thread_goals: boolean
  memory: boolean
  mcp_server_management: boolean
  skill_lifecycle: boolean
  skill_detail: boolean
  plugin_management: boolean
  agent_mail: boolean
  terminal_stream: boolean
  terminal_input: boolean
  terminal_resize: boolean
  terminal_kill: boolean
  event_stream_resume: boolean
  [key: string]: Json | undefined
}

export type RuntimeEventEnvelope = {
  schema_version: number
  seq: number
  event: string
  kind: string
  thread_id: string
  turn_id: (string | null)
  item_id: (string | null)
  timestamp: string
  created_at?: string
  payload: Json
  [key: string]: Json | undefined
}

export type RuntimeExperimentalCapabilities = {
  environments: boolean
  [key: string]: Json | undefined
}

export type RuntimeImageInput = {
  mime: string
  dataBase64: string
}

export type ThreadLoadedListParams = {
  cursor?: (string | null)
  limit?: (number | null)
  [key: string]: Json | undefined
}

export type ThreadLoadedListResponse = {
  data: string[]
  nextCursor: (string | null)
  [key: string]: Json | undefined
}

export type ThreadSetNameParams = {
  threadId: string
  name: string
  [key: string]: Json | undefined
}

export type ThreadSetNameResponse = Record<string, never>

export type TurnEnvironmentParams = {
  environment_id: string
  cwd: string
  [key: string]: Json | undefined
}
