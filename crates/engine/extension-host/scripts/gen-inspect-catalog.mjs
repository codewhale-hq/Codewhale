import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, '..')
const SOURCES = {
  compatibility: join(root, 'src/dsh/compatibility.json'),
  hostRoot: join(root, 'src/root.ts'),
  shellHooks: join(root, 'src/dsh/shell-hooks.ts'),
  seatTable: join(root, '../../app-server/src/runtime_api/plugins.rs'),
  catalog: join(root, 'src/dsh/inspect-catalog.json'),
}

const read = (path) => readFileSync(path, 'utf8').replace(/\r\n/g, '\n')

const SERVICES = {
  tools: {
    summary: 'The tool registry: a plugin adds model-callable tools. The Engine owns calling, approval and results.',
    availability: 'always',
    methods: [
      { name: 'register', signature: 'register(definition: ToolDefinition): () => void', summary: 'Register one tool built with defineTool from @deepseek-ai/dsh-tools. The definition must stay inside the supported JSON Schema subset (type, oneOf, properties, required, additionalProperties, items, enum, const). A second argument is refused. Returns an idempotent disposer.' },
      { name: 'get', signature: 'get(name: string, agent: Agent): { name: string } | undefined', summary: 'Whether the calling agent has a tool by this name. Answers only inside a live tool call, for that call\'s own agent.' },
    ],
  },
  commands: {
    summary: 'Slash commands a person types. A handler runs only when the person types the command.',
    availability: 'always',
    methods: [
      { name: 'register', signature: 'register(definition: { name: string, description: string, argumentHint?: string, handler(call: { args: string, signal: AbortSignal }): CommandResult }): () => void', summary: 'Register one slash command. Names start with a lower case letter, then lower case letters, digits, _ and -, at most 64 characters; at most 64 commands per plugin. A name that clashes with a built-in command gets the alias plugin-<plugin-slug>-<command>, and is refused if that alias is invalid or also taken. A name another plugin already registered is refused, so pick a plugin-specific name. Returns an idempotent disposer.' },
    ],
  },
  storage: {
    summary: 'Owner-local JSON persistence under the data folder the Engine assigned. It holds no session history and no secrets.',
    availability: 'always',
    methods: [
      { name: 'get', signature: 'get(key: string): Promise<Json | undefined>', summary: 'Read one stored value.' },
      { name: 'set', signature: 'set(key: string, value: Json): Promise<void>', summary: 'Store one JSON value, within the bounded key, value and owner limits.' },
      { name: 'delete', signature: 'delete(key: string): Promise<void>', summary: 'Remove one stored value.' },
    ],
  },
  skills: {
    summary: 'Skill roots: reviewed folders of skills added to the Engine\'s skill catalog.',
    availability: 'always',
    methods: [
      { name: 'registerRoot', signature: 'registerRoot(definition: { path: string }): () => void', summary: 'Add a bundle-relative folder of SKILL.md packages. At most 8 roots per plugin. Absolute paths, links, empty components, . and .. are refused.' },
    ],
  },
  sessions: {
    summary: 'The session of the calling tool invocation.',
    availability: 'tool-invocation',
    methods: [
      { name: 'flush', signature: 'flush(session: Session): Promise<boolean>', summary: 'Ask the Engine to persist the committed prefix of the exact session of this tool call. Nothing else on sessions is available.' },
    ],
  },
  userQuestions: {
    summary: 'Questions put to the person through the Engine\'s own question card.',
    availability: 'tool-invocation',
    methods: [
      { name: 'ask', signature: 'ask(request: { agent: Agent, questions: Question[], signal?: AbortSignal }): Promise<Answers>', summary: 'Ask the person typed questions during a live root tool call. Timed or continued questions (wait) are not supported.' },
    ],
  },
  subagents: {
    summary: 'One independent child agent a root tool call may start.',
    availability: 'tool-invocation',
    methods: [
      { name: 'getProvider', signature: 'getProvider(id: \'spawn\'): Provider', summary: 'Describe the one provider: a fresh child with no inherited conversation.' },
      { name: 'startActivation', signature: 'startActivation(options: { provider: \'spawn\', childId: string, request: { parent: Agent, prompt: TextBlock[], agentOptions?: { model?: string } }, signal?: AbortSignal, delivery?: \'caller\' | \'parent\' }): Promise<{ childId: string, messageId: string, result: Promise<ChildResult>, dispose(): Promise<void> }>', summary: 'Start the child through the Engine\'s approval gate. Resolves to the activation handle { childId, messageId, result, dispose }. Prompts are text only; only the model option is accepted. Needs the live calling agent as parent and is not available inside a child.' },
      { name: 'result', signature: 'activation.result: Promise<{ stopReason: string, output: { type: \'text\', text: string }[], diagnostic?: string }>', summary: 'A getter, not a method: read the property, do not call it. Resolves when the child finishes, with its stop reason and its text output blocks; diagnostic notes dropped non-text blocks.' },
      { name: 'dispose', signature: 'activation.dispose(): Promise<void>', summary: 'Stop that exact child and its own descendants.' },
    ],
  },
  agents: {
    summary: 'Reserved. The host offers no methods on agents.',
    availability: 'none',
    methods: [],
  },
  systemPrompt: {
    summary: 'The child agent\'s persona slot, reachable only inside an agent/created listener.',
    availability: 'child-agent-hook',
    methods: [
      { name: 'section', signature: 'agent.ctx.systemPrompt.section({ name: \'deployment:persona-prefix\', order: agent.ctx.systemPrompt.getSectionOrder(\'DEPLOYMENT_PERSONA_PREFIX\'), text: string }): () => void', summary: 'Set the child\'s persona text, at most 32 KiB, once. name must be the string deployment:persona-prefix (the PERSONA_PREFIX_SECTION value) and order must be what getSectionOrder(\'DEPLOYMENT_PERSONA_PREFIX\') returns, which is 0. Anything else is refused. Called anywhere else it fails. The Engine\'s own system prompt cannot be replaced.' },
    ],
  },
  logger: {
    summary: 'Cordis logging. Every line becomes a host log notification attributed to the plugin.',
    availability: 'always',
    cordis: true,
    methods: [
      { name: 'logger', signature: 'ctx.logger(name?: string): Logger', summary: 'A named logger with debug, info, warn and error. The host reports error and warn as such and everything else as info.' },
    ],
  },
  events: {
    summary: 'Cordis event dispatch. Query the host Event provider for the events this host fires.',
    availability: 'always',
    cordis: true,
    methods: [
      { name: 'on', signature: 'ctx.on(name: string, listener: Function, options?: boolean | { prepend?: boolean, global?: boolean }): () => boolean', summary: 'Listen to an event. The host refuses five lifecycle events and limits the options of two (see the Event provider).' },
      { name: 'once', signature: 'ctx.once(name: string, listener: Function, options?: boolean | { prepend?: boolean, global?: boolean }): () => boolean', summary: 'Listen to an event once.' },
      { name: 'emit', signature: 'ctx.emit(name: string, ...args: any[]): void', summary: 'Dispatch an event to its listeners.' },
    ],
  },
  reflect: {
    summary: 'Cordis service reflection. A plugin may provide its own services but not the Engine\'s.',
    availability: 'always',
    cordis: true,
    methods: [
      { name: 'provide', signature: 'ctx.reflect.provide(name: string, value?: any): () => void', summary: 'Provide a service under a name. Names in refusedServices are refused: the Engine owns them.' },
      { name: 'get', signature: 'ctx.reflect.get(name: string, strict?: boolean): any', summary: 'Read a provided service.' },
    ],
  },
  registry: {
    summary: 'Cordis plugin registry.',
    availability: 'always',
    cordis: true,
    methods: [
      { name: 'get', signature: 'ctx.registry.get(plugin: Plugin): Runtime | undefined', summary: 'Look up the runtime record of a plugin.' },
      { name: 'resolve', signature: 'ctx.registry.resolve(plugin: Plugin): Function | undefined', summary: 'Resolve a plugin shape to its callback.' },
    ],
  },
  prompt: {
    summary: 'Attributed sections of the Engine\'s runtime prompt. A plugin cannot replace the system prompt.',
    availability: 'always',
    methods: [
      { name: 'registerSection', signature: 'registerSection(definition: { id: string, text: string, interpolate?: \'model-cwd\' }): () => void', summary: 'Contribute a plain-text section of at most 4 KiB; at most 128 sections per plugin. The id is lower case letters, digits, _ and -, starts with a letter, at most 64 characters. With interpolate set, only {{model}} and {{cwd}} are expanded.' },
    ],
  },
  avatars: {
    summary: 'Avatar packs for the terminal.',
    availability: 'always',
    methods: [
      { name: 'registerPack', signature: 'registerPack(definition: { path: string }): () => void', summary: 'Register a reviewed bundle-relative avatar pack: a path ending in .json, at most 4 packs per plugin.' },
    ],
  },
  mcp: {
    summary: 'Proposals of MCP servers. The Engine owns connection, credentials and tool calls.',
    availability: 'native-review',
    methods: [
      { name: 'registerServer', signature: 'registerServer(definition: { serverName: string, server: Record<string, unknown> }): () => void', summary: 'Propose one server. serverName is a letter or digit followed by up to 31 letters, digits, _ or -. headers and env must stay empty and the url may carry no credentials, query or fragment. At most 64 servers per plugin; at most 64 KiB per definition.' },
    ],
  },
  shellHooks: {
    summary: 'Claude Code and Codex style command hooks, run by the Engine.',
    availability: 'native-review',
    methods: [
      { name: 'register', signature: 'register(definition: { dialect: \'claude-code\' | \'codex\', point: string, matcher?: string, hook: { event: string, command: string, timeout_secs: number, background: false, continue_on_error: false, name: string } }): () => void', summary: 'Register one command hook for a reviewed native plugin. Forced continuation, observer steering, non-command and asynchronous hooks are not supported.' },
    ],
  },
  remote: {
    summary: 'Remote pairing, served by the Engine.',
    availability: 'review-grant',
    methods: [
      { name: 'status', signature: 'status(): Promise<Status>', summary: 'Whether remote access is on.' },
      { name: 'pair', signature: 'pair(options?: { label?: string }): Promise<Pairing>', summary: 'Start a pairing.' },
      { name: 'clients', signature: 'clients(): Promise<Client[]>', summary: 'List paired clients.' },
      { name: 'revoke', signature: 'revoke(id: string): Promise<void>', summary: 'Revoke one grant by the id pair returned.' },
    ],
  },
}

const AVAILABILITY = {
  always: 'Available to every plugin that injects it.',
  'tool-invocation': 'Answers only inside a live tool call of the plugin.',
  'child-agent-hook': 'Answers only inside an agent/created listener, on the child agent.',
  'native-review': 'Available to a reviewed native plugin.',
  'review-grant': 'Served by the Engine only when the plugin\'s review granted it.',
  none: 'No methods.',
}

const EVENTS_SUPPORTED = [
  {
    name: 'tools/pre-execute',
    status: 'supported',
    mode: 'ordered listeners, native hooks first, then plugin listeners in registration order',
    signature: 'ctx.on(\'tools/pre-execute\', async (exec: { name: string, callId: string, arguments: object, signal: AbortSignal, workspace: string, mode: string, model: string }, next: () => Promise<Abstention>) => Verdict | undefined)',
    summary: 'Programmable tool admission for a reviewed native plugin. The frozen call view has no session, agent, tool or approval handle. Return { kind: \'deny\', reason }, { kind: \'ask\', reason? }, { kind: \'revise\', input }, { kind: \'annotate\', text } or { kind: \'abstain\' }; undefined abstains. A malformed answer, error or timeout fails the call closed. The listener batch has a five second deadline.',
    options: 'prepend and global are refused.',
  },
  {
    name: 'agent/created',
    status: 'supported',
    mode: 'listeners of the owning plugin only',
    signature: 'ctx.on(\'agent/created\', async ({ agent }: { agent: { id: string, ctx: { systemPrompt: { getSectionOrder(name), section(section) }, tools: { restrict(filter: { deny: string[] }) } } } }) => void)',
    summary: 'Fires for a child agent that this plugin\'s own subagents.startActivation creates, before the child starts. The listener may set the child\'s persona and deny tools to it; the window closes when the listeners return.',
    options: 'global is refused.',
  },
]

const REFUSED_EVENT_REASON = 'Not projected by this host: no core projection exists, so a listener would never fire.'

const ROW_PACKAGES = {
  '@deepseek-ai/dsh-mcp-client': {
    summary: 'An MCP server row. The Engine proposes the server and owns the connection.',
    schema: {
      type: 'object',
      additionalProperties: false,
      required: ['transport', 'serverName'],
      properties: {
        transport: { type: 'string', enum: ['stdio', 'streamable-http', 'sse'] },
        serverName: { type: 'string', pattern: '^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$' },
        command: { type: 'string', description: 'stdio only: the program to run.' },
        args: { type: 'array', items: { type: 'string' }, description: 'stdio only.' },
        env: { type: 'object', maxProperties: 0, description: 'Must be empty or absent: credentials stay with the Engine.' },
        cwd: { type: 'string', description: 'stdio only: a bundle-relative directory, no .. components.' },
        url: { type: 'string', description: 'streamable-http and sse: the endpoint, with no credentials, query or fragment.' },
        headers: { type: 'object', maxProperties: 0, description: 'Must be empty or absent: credentials stay with the Engine.' },
        toolCallTimeoutMs: { type: 'integer', minimum: 1000, maximum: 86400000, multipleOf: 1000 },
        failOnStartupError: { type: 'boolean', const: false, description: 'Only false: the Engine connects lazily and reports failures through /mcp.' },
      },
    },
    limits: ['stdio rows name a command; streamable-http and sse rows name a url.', 'Reconnect and instruction bounds belong to the Engine and are not configurable here.'],
    example: { transport: 'stdio', serverName: 'docs', command: 'node', args: ['server.js'], cwd: '.' },
  },
  '@deepseek-ai/dsh-skill-filesystem': {
    summary: 'A skill folder row, limited to reviewed roots inside the bundle.',
    schema: {
      type: 'object',
      additionalProperties: false,
      required: ['includeDefaultRoots', 'watch', 'customSkillDirs'],
      properties: {
        providerName: { type: 'string', maxLength: 64 },
        includeDefaultRoots: { type: 'boolean', const: false, description: 'Must be false: ambient roots are not read.' },
        watch: { type: 'boolean', const: false, description: 'Must be false: there is no file watcher.' },
        customSkillDirs: { type: 'array', minItems: 1, maxItems: 8, items: { type: 'string' }, description: 'Bundle-relative skill roots, no duplicates.' },
      },
    },
    limits: ['Both includeDefaultRoots and watch must be written out as false.'],
    example: { includeDefaultRoots: false, watch: false, customSkillDirs: ['skills'] },
  },
  '@deepseek-ai/dsh-hooks-claude-code': {
    summary: 'Claude Code style command hooks read from a reviewed file in the bundle.',
    schema: {
      type: 'object',
      additionalProperties: true,
      required: ['configPath'],
      properties: {
        configPath: { type: 'string', description: 'A regular file inside the bundle, at most 1 MiB, unchanged since review.' },
        pluginRoot: { type: 'string', description: 'Absent, ".", or this reviewed bundle root.' },
        defaultTimeoutMs: { type: 'integer', minimum: 1000, maximum: 86400000, multipleOf: 1000, description: 'Whole seconds, used when a hook sets none; default 600000.' },
      },
    },
    limits: ['projectDir is refused: each process uses the current workspace.', 'Only command hooks run; non-command and asynchronous hooks fail the row.', 'At most 128 commands per row.'],
    example: { configPath: 'hooks/hooks.json' },
  },
  '@deepseek-ai/dsh-hooks-codex': {
    summary: 'Codex style command hooks read from a reviewed file in the bundle.',
    schema: {
      type: 'object',
      additionalProperties: true,
      required: ['configPath'],
      properties: {
        configPath: { type: 'string', description: 'A regular file inside the bundle, at most 1 MiB, unchanged since review.' },
        pluginRoot: { type: 'string', description: 'Absent, ".", or this reviewed bundle root.' },
        defaultTimeoutMs: { type: 'integer', minimum: 1000, maximum: 86400000, multipleOf: 1000, description: 'Whole seconds, used when a hook sets none; default 600000.' },
      },
    },
    limits: ['projectDir is refused: each process uses the current workspace.', 'Only command hooks run; non-command and asynchronous hooks fail the row.', 'At most 128 commands per row.'],
    example: { configPath: 'hooks/hooks.json' },
  },
  '@deepseek-ai/dsh-persona': {
    summary: 'Text added before and after the Engine\'s own prompt for this plugin\'s entry.',
    schema: {
      type: 'object',
      additionalProperties: false,
      required: ['prefix'],
      properties: {
        prefix: { type: 'string', description: 'At most 4096 UTF-8 bytes, no control characters. Only {{model}} and {{cwd}} are expanded.' },
        suffix: { type: 'string', description: 'Same limits as prefix.' },
        complete: { type: 'boolean', const: false, description: 'Replacing the whole prompt belongs to the Engine.' },
        includeRuntimeContext: { type: 'boolean', const: true, description: 'Suppressing the runtime context belongs to the Engine.' },
      },
    },
    limits: ['Needs an exact Engine-selected entry scope.'],
    example: { prefix: 'Answer in short sentences.' },
  },
}

const THEME = {
  tokens: [],
  colorSchemes: ['light', 'dark'],
  scheme: 'The frame sets data-theme and color-scheme on its document to the app\'s current scheme and follows changes.',
  api: [
    { name: 'ctx.theme.getTheme', signature: 'getTheme(): { active: { colorScheme: \'light\' | \'dark\' } }', summary: 'The scheme in use now.' },
    { name: 'ctx.theme.overrideTokens', signature: 'overrideTokens(source: string, tokens: Record<string, { light: string, dark: string }>): () => void', summary: 'Set custom properties on the frame\'s document for each scheme. source is at most 256 characters; each name starts with -- and is at most 128 characters; each value is at most 512 characters. Returns a disposer.' },
  ],
  notes: [
    'The frame defines no named design tokens of its own. There are no --dsw-* tokens to read: a plugin that wants tokens defines them with overrideTokens and uses them in its own styles.',
    'The frame\'s background is transparent and its margin is zero, so the plugin sits on the app\'s own surface.',
  ],
}

const SEAT_NOTES = {
  'customize.plugins.detail': { drawn: true, where: 'The plugin\'s own page under Customize, Plugins.' },
  'conversation.composer.dock': { drawn: false, where: 'Under the composer. The desktop app does not place it yet.' },
  'conversation.chat.node': { drawn: false, where: 'Inside the conversation. The desktop app does not place it yet.' },
  'workpane.panel': { drawn: false, where: 'The work pane. The desktop app does not place it yet.' },
  'sidebar.section': { drawn: false, where: 'The sidebar. The desktop app does not place it yet.' },
  'command.row': { drawn: false, where: 'The command list. The desktop app does not place it yet.' },
}

function between(text, open, close, what) {
  const start = text.indexOf(open)
  if (start < 0) throw new Error(`${what}: cannot find ${open}`)
  const end = text.indexOf(close, start + open.length)
  if (end < 0) throw new Error(`${what}: cannot find the end of ${open}`)
  return text.slice(start + open.length, end)
}

export function parseSeatTable(source) {
  const text = source.replace(/\r\n/g, '\n')
  const constants = Object.fromEntries([...text.matchAll(/const ([A-Z_]+): &str = "([^"]+)";/g)].map((m) => [m[1], m[2]]))
  const resolve = (literal, name) => literal !== undefined ? literal : constants[name]
  const seatBlock = between(text, 'const PLUGIN_UI_SEATS: [&str; ', '];', 'seat list')
  const declaredSeats = Number(seatBlock.slice(0, seatBlock.indexOf(']')))
  const seatBody = seatBlock.slice(seatBlock.indexOf('= [') + 3)
  const seats = [...seatBody.matchAll(/"([^"]+)"|([A-Z_]+)/g)].map((m) => resolve(m[1], m[2]))
  if (seats.length !== declaredSeats || seats.some((seat) => seat === undefined)) throw new Error('the seat list does not match its declared length')
  const tableBlock = between(text, 'const DSH_SEAT_TABLE: [(&str, DshSeat); ', '];\n', 'slot table')
  const declaredSlots = Number(tableBlock.slice(0, tableBlock.indexOf(']')))
  const tableBody = tableBlock.slice(tableBlock.indexOf('= [') + 3)
  const slots = [...tableBody.matchAll(/\(\s*"([^"]+)",\s*DshSeat::(Seat|Unsupported)\(\s*(?:"([^"]*)"|([A-Z_]+))\s*\)\s*,?\s*\)/g)].map((m) => {
    const value = resolve(m[3], m[4])
    return m[2] === 'Seat' ? { slot: m[1], seat: value, supported: true } : { slot: m[1], seat: null, supported: false, reason: value }
  })
  if (slots.length !== declaredSlots) throw new Error(`the slot table declares ${declaredSlots} rows and ${slots.length} were read`)
  for (const slot of slots) if (slot.supported && !seats.includes(slot.seat)) throw new Error(`slot ${slot.slot} maps to ${slot.seat}, which is not a seat`)
  return { seats, slots }
}

export function parseRefusedEvents(source) {
  const text = source.replace(/\r\n/g, '\n')
  const body = between(text, 'const unsupportedDshLifecycle = new Set([', '])', 'refused events')
  return [...body.matchAll(/'([^']+)'/g)].map((m) => m[1])
}

export function parseHookEvents(source) {
  const text = source.replace(/\r\n/g, '\n')
  const body = between(text, 'const EVENTS:Record<string,string>={', '}', 'hook events')
  return [...body.matchAll(/([A-Za-z]+):'([a-z_]+)'/g)].map((m) => ({ event: m[1], point: m[2] }))
}

export function buildCatalog(paths = SOURCES) {
  const compatibility = JSON.parse(read(paths.compatibility))
  const provided = Object.entries(compatibility.provided_services).flatMap(([group, names]) => names.map((name) => ({ group, name })))
  const known = new Set(Object.keys(SERVICES))
  const missing = provided.filter(({ name }) => !known.has(name)).map(({ name }) => name)
  if (missing.length) throw new Error(`compatibility.json provides services with no entry in the method table: ${missing.join(', ')}`)
  const extra = [...known].filter((name) => !provided.some((entry) => entry.name === name))
  if (extra.length) throw new Error(`the method table names services the host does not provide: ${extra.join(', ')}`)

  const services = provided.map(({ group, name }) => {
    const entry = SERVICES[name]
    return {
      key: name,
      group,
      summary: entry.summary,
      availability: entry.availability,
      availabilityText: AVAILABILITY[entry.availability],
      methods: entry.methods,
    }
  })

  const refusedEvents = parseRefusedEvents(read(paths.hostRoot))
  const hookEvents = parseHookEvents(read(paths.shellHooks))
  const events = [
    ...EVENTS_SUPPORTED,
    ...refusedEvents.map((name) => ({ name, status: 'refused', summary: REFUSED_EVENT_REASON })),
  ]

  const rowPackages = Object.keys(compatibility.row_bridges)
  const unknownRows = rowPackages.filter((name) => !ROW_PACKAGES[name])
  const staleRows = Object.keys(ROW_PACKAGES).filter((name) => !rowPackages.includes(name))
  if (unknownRows.length || staleRows.length) throw new Error(`row packages differ from compatibility.json: missing ${unknownRows.join(', ')}; stale ${staleRows.join(', ')}`)
  const rows = rowPackages.map((name) => ({
    id: `row:${name}`,
    name,
    bridge: compatibility.row_bridges[name],
    summary: ROW_PACKAGES[name].summary,
    limits: ROW_PACKAGES[name].limits,
    example: ROW_PACKAGES[name].example,
    ...(name.includes('hooks-') ? { hookEvents } : {}),
    schema: { $schema: 'https://json-schema.org/draft/2020-12/schema', title: name, ...ROW_PACKAGES[name].schema },
  }))

  const table = parseSeatTable(read(paths.seatTable))
  const unknownSeatNotes = table.seats.filter((seat) => !SEAT_NOTES[seat])
  const staleSeatNotes = Object.keys(SEAT_NOTES).filter((seat) => !table.seats.includes(seat))
  if (unknownSeatNotes.length || staleSeatNotes.length) throw new Error(`seat notes differ from the Engine seat list: missing ${unknownSeatNotes.join(', ')}; stale ${staleSeatNotes.join(', ')}`)
  const seats = table.seats.map((seat) => ({ seat, drawn: SEAT_NOTES[seat].drawn, where: SEAT_NOTES[seat].where }))

  return {
    version: 1,
    targetDshVersion: compatibility.target_dsh_version,
    host: {
      services,
      refusedServices: compatibility.refused_services,
      events,
      cordisInternalEvents: 'Cordis fires its own internal/* lifecycle events. They are not part of this host\'s supported contract and are not listed.',
      config: rows,
    },
    client: {
      seats,
      slots: table.slots,
      theme: THEME,
    },
  }
}

export function renderCatalog(catalog) {
  return `${JSON.stringify(catalog, null, 2)}\n`
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const text = renderCatalog(buildCatalog())
  if (process.argv.includes('--check')) {
    const current = existsOrEmpty(SOURCES.catalog)
    if (current !== text) {
      process.stderr.write('inspect-catalog.json is out of date: run node scripts/gen-inspect-catalog.mjs\n')
      process.exit(1)
    }
  } else {
    writeFileSync(SOURCES.catalog, text)
  }
}

function existsOrEmpty(path) {
  try {
    return read(path)
  } catch {
    return ''
  }
}
