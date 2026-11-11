// A deterministic Agent Client Protocol agent that advertises its own models,
// a mode, a config option and one control Codewhale cannot write, and reports
// token usage. Behavior is steered by `options-fixture.json` in the session
// cwd; everything it receives is appended to `options-received.jsonl` there.
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const plan = () => {
  const path = join(process.cwd(), 'options-fixture.json');
  return existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : {};
};
const record = (entry) =>
  appendFileSync(join(process.cwd(), 'options-received.jsonl'), `${JSON.stringify({ pid: process.pid, ...entry })}\n`);
writeFileSync(join(process.cwd(), 'options-env.json'), JSON.stringify(process.env));

const MODELS = [
  { value: 'model-a', name: 'Model A', description: 'Fast' },
  { value: 'model-b', name: 'Model B', description: 'Careful' },
  { value: 'model-c', name: 'Model C' },
];
const EFFORTS = [
  { value: 'low', name: 'Low' },
  { value: 'high', name: 'High' },
];
const MODES = [
  { id: 'default', name: 'Default' },
  { id: 'review', name: 'Review only', description: 'Reads, never edits' },
];

let buffer = '';
let sessionId = null;
const state = { model: 'model-a', effort: 'low', mode: 'default' };

const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);
const result = (id, value) => send({ jsonrpc: '2.0', id, result: value });
const failure = (id, code, message) => send({ jsonrpc: '2.0', id, error: { code, message } });
const notify = (method, params) => send({ jsonrpc: '2.0', method, params });

function offer() {
  const withdrawn = new Set(plan().withdraw ?? []);
  const models = MODELS.filter((model) => !withdrawn.has(model.value));
  const configOptions = [
    { id: 'model', name: 'Model', category: 'model', type: 'select', currentValue: state.model, options: models },
    { id: 'effort', name: 'Reasoning effort', category: 'thought_level', type: 'select', currentValue: state.effort, options: EFFORTS },
    { id: 'verbose', name: 'Verbose logging', type: 'boolean', currentValue: false },
  ];
  const reply = {
    configOptions,
    modes: { currentModeId: state.mode, availableModes: MODES },
  };
  if (plan().malformed) {
    reply.configOptions.push({ id: '', name: 'No id', type: 'select', currentValue: 'x', options: [] });
    reply.configOptions.push({ id: 'empty', name: 'Empty', type: 'select', currentValue: 'x', options: [] });
  }
  if (plan().huge) {
    reply.configOptions.push({
      id: 'many',
      name: 'Many',
      type: 'select',
      currentValue: 'v0',
      options: Array.from({ length: 600 }, (_, index) => ({ value: `v${index}`, name: `Value ${index}` })),
    });
  }
  return reply;
}

function offered(config, value) {
  const found = offer().configOptions.find((option) => option.id === config);
  return Boolean(found && found.type === 'select' && found.options.some((option) => option.value === value));
}

function prompt(request) {
  const text = (request.params?.prompt ?? []).map((block) => block.text ?? '').join('');
  record({ method: 'session/prompt', text, state: { ...state } });
  notify('session/update', {
    sessionId,
    update: {
      sessionUpdate: 'agent_message_chunk',
      content: { type: 'text', text: `model=${state.model} effort=${state.effort} mode=${state.mode}` },
    },
  });
  if (!plan().no_usage) {
    notify('session/update', { sessionId, update: { sessionUpdate: 'usage_update', used: 1200, size: 200000 } });
  }
  result(request.id, {
    stopReason: 'end_turn',
    ...(plan().no_usage ? {} : { usage: { inputTokens: 100, outputTokens: 50, totalTokens: 150, thoughtTokens: 7 } }),
  });
}

function handle(message) {
  if (message.id !== undefined && message.method === undefined) return;
  const params = message.params ?? {};
  switch (message.method) {
    case 'initialize':
      result(message.id, {
        protocolVersion: 1,
        agentInfo: { name: 'options-fixture', version: '1.0.0' },
        authMethods: [],
        agentCapabilities: {
          loadSession: true,
          promptCapabilities: { image: true, embeddedContext: true },
          sessionCapabilities: { fork: {} },
        },
      });
      break;
    case 'session/new':
      if (plan().auth_required) {
        failure(message.id, -32000, 'Authentication required');
        break;
      }
      sessionId = `options-session-${process.pid}`;
      record({ method: 'session/new', cwd: params.cwd });
      if (plan().hang) break;
      result(message.id, { sessionId, ...offer() });
      break;
    case 'session/load':
    case 'session/resume':
      sessionId = params.sessionId ?? null;
      record({ method: message.method, sessionId });
      result(message.id, offer());
      break;
    case 'session/set_config_option':
      record({ method: message.method, configId: params.configId, value: params.value });
      if (!offered(params.configId, params.value)) {
        failure(message.id, -32602, 'value not offered');
        break;
      }
      state[params.configId] = params.value;
      result(message.id, { configOptions: offer().configOptions });
      break;
    case 'session/set_mode':
      record({ method: message.method, modeId: params.modeId });
      if (!MODES.some((mode) => mode.id === params.modeId)) {
        failure(message.id, -32602, 'mode not offered');
        break;
      }
      state.mode = params.modeId;
      result(message.id, {});
      break;
    case 'session/prompt':
      prompt(message);
      break;
    case 'session/cancel':
      break;
    default:
      if (message.id !== undefined) failure(message.id, -32601, `no method ${message.method}`);
  }
}

process.stdin.on('data', (chunk) => {
  buffer += chunk;
  let index;
  while ((index = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, index);
    buffer = buffer.slice(index + 1);
    if (!line.trim()) continue;
    try {
      handle(JSON.parse(line));
    } catch (error) {
      process.stderr.write(`options fixture: bad line: ${String(error)}\n`);
    }
  }
});
process.stdin.on('end', () => process.exit(0));
