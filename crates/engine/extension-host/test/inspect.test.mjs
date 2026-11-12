import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { build } from 'esbuild'
import { dirname, join } from 'node:path'
import { after, test } from 'node:test'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { buildCatalog, parseSeatTable, renderCatalog } from '../scripts/gen-inspect-catalog.mjs'
import { createCordisInspect } from '../src/dsh/inspect.ts'

const here = dirname(fileURLToPath(import.meta.url))
const src = (path) => join(here, '..', 'src', path)
const text = (path) => readFileSync(path, 'utf8').replace(/\r\n/g, '\n')
const compatibility = JSON.parse(text(src('dsh/compatibility.json')))
const catalog = JSON.parse(text(src('dsh/inspect-catalog.json')))
const seatSource = text(join(here, '..', '..', '..', 'app-server', 'src', 'runtime_api', 'plugins.rs'))
const rootSource = text(src('root.ts'))
const inspect = createCordisInspect()
const ask = (platform, provider, method, input) => inspect.query(platform, provider, method, input)

test('the committed catalog is what the generator writes now', () => {
  assert.equal(text(src('dsh/inspect-catalog.json')), renderCatalog(buildCatalog()))
})

test('list names the providers DSH calls and leaves out Tool until a source exists', async () => {
  const ids = inspect.list().map((p) => `${p.platform}.${p.id}.${p.methods.map((m) => m.name).join(',')}`)
  assert.deepEqual(ids, ['host.Service.listService', 'host.Event.listEvents', 'host.Config.listConfigs', 'client.Slots.listSubTree', 'client.Theme.listTokens'])
  const withTools = createCordisInspect({ tools: () => [{ name: 'read_file' }] })
  assert.ok(withTools.list().some((p) => p.id === 'Tool'))
  assert.deepEqual(await withTools.query('host', 'Tool', 'listTools', undefined, {}), { tools: [{ name: 'read_file' }] })
})

test('errors use DSH registry wording', async () => {
  await assert.rejects(ask('host', 'Nope', 'x'), /Host Cordis inspect provider "Nope" is not registered/)
  await assert.rejects(ask('client', 'Nope', 'x'), /Client Cordis inspect provider "Nope" is not registered/)
  await assert.rejects(ask('host', 'Service', 'nope'), /Cordis inspect provider "Service" has no method "nope"/)
  await assert.rejects(ask('host', 'Service', 'listService', { service: 3 }), /Host Cordis inspect Service.listService rejected input/)
  await assert.rejects(ask('host', 'Tool', 'listTools'), /not registered/)
  await assert.rejects(ask('page', 'Service', 'listService'), /platform must be/)
})

test('every service the catalog names is provided, and no provided service is missing', () => {
  const provided = Object.values(compatibility.provided_services).flat().sort()
  assert.deepEqual(catalog.host.services.map((s) => s.key).sort(), provided)
  assert.deepEqual(catalog.host.refusedServices, compatibility.refused_services)
})

test('methods match the compatibility tables where it has them', () => {
  const tables = { sessions: 'session_methods', userQuestions: 'user_question_methods', subagents: 'subagent_methods', agents: 'agents_methods', systemPrompt: 'system_prompt_methods' }
  for (const [service, table] of Object.entries(tables)) {
    const named = catalog.host.services.find((s) => s.key === service).methods.map((m) => m.name).sort()
    assert.deepEqual(named, Object.keys(compatibility[table]).sort(), service)
  }
})

test('every curated method exists in the source that implements it', () => {
  const files = {
    tools: 'root.ts', commands: 'shims/commands.ts', storage: 'shims/storage.ts', skills: 'shims/skills.ts', prompt: 'shims/prompt.ts',
    avatars: 'shims/avatars.ts', mcp: 'shims/mcp.ts', shellHooks: 'shims/shell-hooks.ts', remote: 'dsh/remote.ts', sessions: 'dsh/session-prompt.ts',
    userQuestions: 'root.ts', subagents: 'dsh/agents.ts', systemPrompt: 'dsh/session-prompt.ts',
  }
  const cordis = { events: 'events', reflect: 'reflect', registry: 'registry' }
  for (const service of catalog.host.services) {
    for (const m of service.methods) {
      if (service.key === 'logger') continue
      let body
      if (cordis[service.key]) body = text(join(here, '..', 'node_modules', '@deepseek-ai', 'cordis', 'lib', 'types', `${cordis[service.key]}.d.ts`))
      else if (service.key === 'agents') continue
      else body = text(src(files[service.key]))
      const pattern = new RegExp(`\\b${m.name}\\s*[(<]|\\b${m.name}\\s*:\\s*(async\\s*)?(function|\\()`)
      assert.match(body, pattern, `${service.key}.${m.name}`)
    }
  }
})

test('every slot and its seat equal the Engine seat table', () => {
  const block = seatSource.slice(seatSource.indexOf('const DSH_SEAT_TABLE'), seatSource.indexOf('];', seatSource.indexOf('const DSH_SEAT_TABLE')))
  const names = [...block.matchAll(/\(\s*"([^"]+)",\s*DshSeat::/g)].map((m) => m[1])
  assert.equal(names.length, 30)
  assert.deepEqual(catalog.client.slots.map((s) => s.slot), names)
  const unsupported = [...block.matchAll(/\(\s*"([^"]+)",\s*DshSeat::Unsupported/g)].map((m) => m[1])
  assert.deepEqual(catalog.client.slots.filter((s) => !s.supported).map((s) => s.slot), unsupported)
  const seats = [...seatSource.slice(seatSource.indexOf('const PLUGIN_UI_SEATS'), seatSource.indexOf('];', seatSource.indexOf('const PLUGIN_UI_SEATS'))).matchAll(/"([^"]+)"/g)].map((m) => m[1])
  for (const slot of catalog.client.slots.filter((s) => s.supported)) assert.ok(slot.seat === 'customize.plugins.detail' || seats.includes(slot.seat), slot.slot)
  assert.equal(catalog.client.seats.length, 6)
  assert.deepEqual(parseSeatTable(seatSource.replace(/\n/g, '\r\n')), parseSeatTable(seatSource))
})

test('the refused events equal the list in root.ts', () => {
  const body = rootSource.slice(rootSource.indexOf('unsupportedDshLifecycle = new Set(['), rootSource.indexOf('])', rootSource.indexOf('unsupportedDshLifecycle = new Set([')))
  const refused = [...body.matchAll(/'([^']+)'/g)].map((m) => m[1])
  assert.equal(refused.length, 5)
  assert.deepEqual(catalog.host.events.filter((e) => e.status === 'refused').map((e) => e.name), refused)
  for (const name of catalog.host.events.filter((e) => e.status === 'supported').map((e) => e.name)) assert.ok(rootSource.includes(`'${name}'`) || text(src('dsh/agents.ts')).includes(`'${name}'`), name)
})

test('the Service provider answers a directory, one contract, and refuses a name the host lacks', async () => {
  const directory = await ask('host', 'Service', 'listService')
  assert.equal(directory.services.length, 18)
  assert.ok(!directory.services.some((s) => s.key === 'fs' || s.key === 'llm'))
  const one = await ask('host', 'Service', 'listService', { service: 'prompt' })
  assert.equal(one.service.methods[0].name, 'registerSection')
  await assert.rejects(ask('host', 'Service', 'listService', { service: 'llm' }), /not provided by this host/)
})

test('the Event provider separates supported from refused', async () => {
  const { events } = await ask('host', 'Event', 'listEvents')
  assert.deepEqual(events.map((e) => e.status), ['supported', 'supported', 'refused', 'refused', 'refused', 'refused', 'refused'])
  assert.equal((await ask('host', 'Event', 'listEvents', { event: 'tools/pre-execute' })).event.name, 'tools/pre-execute')
  await assert.rejects(ask('host', 'Event', 'listEvents', { event: 'subagent/start-x' }), /not known/)
})

test('the Config provider lists exactly the row packages, pages, and shows installed rows', async () => {
  const page = await ask('host', 'Config', 'listConfigs')
  assert.deepEqual(page.entries.map((e) => e.name).sort(), Object.keys(compatibility.row_bridges).sort())
  assert.equal(page.total, 5)
  const paged = await ask('host', 'Config', 'listConfigs', { limit: 2 })
  assert.equal(paged.entries.length, 2)
  assert.equal(paged.nextOffset, 2)
  const schema = await ask('host', 'Config', 'listConfigs', { entry: 'row:@deepseek-ai/dsh-persona' })
  assert.deepEqual(schema.schema.required, ['prefix'])
  await assert.rejects(ask('host', 'Config', 'listConfigs', { limit: 500 }), /limit/)
  await assert.rejects(ask('host', 'Config', 'listConfigs', { entry: 'row:nope' }), /not listed/)
  const live = createCordisInspect({ installed: () => [{ id: 'abc', name: 'hello', state: 'active' }] })
  const after = await live.query('host', 'Config', 'listConfigs', { name: 'hello' })
  assert.deepEqual(after.entries, [{ id: 'plugin:abc', name: 'hello', kind: 'installed-plugin', status: 'active' }])
})

test('each row example passes the bridge that reads it', async () => {
  const dir = mkdtempSync(join(here, '..', 'node_modules', '.cw-inspect-'))
  after(() => rmSync(dir, { recursive: true, force: true }))
  const bridges = join(dir, 'bridges.mjs')
  await build({
    stdin: { contents: "export { normalizePersonaConfig } from './src/dsh/persona.ts'\nexport { reviewedMcpModule } from './src/dsh/mcp.ts'\nexport { reviewedSkillModule } from './src/dsh/skill-filesystem.ts'", resolveDir: join(here, '..'), loader: 'ts' },
    outfile: bridges, bundle: true, platform: 'node', format: 'esm', packages: 'external', absWorkingDir: join(here, '..'), logLevel: 'silent',
  })
  const { normalizePersonaConfig, reviewedMcpModule, reviewedSkillModule } = await import(pathToFileURL(bridges).href)
  const example = (name) => catalog.host.config.find((r) => r.name === name).example
  assert.doesNotThrow(() => normalizePersonaConfig(example('@deepseek-ai/dsh-persona')))
  assert.throws(() => normalizePersonaConfig({ prefix: 'x', complete: true }))
  const servers = []
  reviewedMcpModule().apply({ mcp: { registerServer: (d) => servers.push(d) } }, example('@deepseek-ai/dsh-mcp-client'))
  assert.equal(servers[0].serverName, 'docs')
  const roots = []
  reviewedSkillModule().apply({ skills: { registerRoot: (d) => roots.push(d) } }, example('@deepseek-ai/dsh-skill-filesystem'))
  assert.equal(roots.length, 1)
})

test('Slots says no client is connected in a terminal and marks no slot drawn', async () => {
  const out = await ask('client', 'Slots', 'listSubTree')
  assert.equal(out.client.connected, false)
  assert.match(out.client.message, /No client is connected/)
  assert.equal(out.trees.length, 30)
  assert.ok(out.trees.every((s) => s.drawn === false))
  assert.deepEqual((await ask('client', 'Slots', 'listSubTree', { root: 'conversation.composer.dock' })).selected.seat, 'conversation.composer.dock')
  assert.equal((await ask('client', 'Slots', 'listSubTree', { root: 'nope' })).requestedRoot.available, false)
})

test('Slots marks only the drawn seats when a client is connected', async () => {
  const connected = createCordisInspect({ client: () => ({ connected: true }) })
  const out = await connected.query('client', 'Slots', 'listSubTree')
  const drawn = out.trees.filter((s) => s.drawn).map((s) => s.name)
  assert.deepEqual(drawn, ['plugins.bundle.config', 'plugins.row.config', 'plugins.tab', 'plugins.detail.section', 'settings.section', 'settings.plugin.item', 'settings.general.item', 'settings.plugins.tab'])
  const all = createCordisInspect({ client: () => ({ connected: true, drawnSeats: ['conversation.composer.dock'] }) })
  const composer = (await all.query('client', 'Slots', 'listSubTree')).trees.filter((s) => s.drawn).map((s) => s.name)
  assert.ok(composer.includes('conversation.input.dock') && !composer.includes('plugins.tab'))
  assert.ok((await ask('client', 'Slots', 'listSubTree')).trees.filter((s) => !s.supported).every((s) => s.seat === null && s.reason))
})

test('Theme reports that the frame defines no named tokens', async () => {
  const out = await ask('client', 'Theme', 'listTokens')
  assert.deepEqual(out.tokens, [])
  assert.deepEqual(out.colorSchemes, ['light', 'dark'])
  assert.ok(out.api.some((a) => a.name === 'ctx.theme.overrideTokens'))
})
