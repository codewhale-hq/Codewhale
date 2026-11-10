// A deterministic Agent Client Protocol agent for the fleet transport test.
// Speaks newline-framed JSON-RPC on stdio. No dependencies, no network.
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';

const HANG = process.env.HANG === '1';
const envPath = join(process.cwd(), 'fixture-env.json');
writeFileSync(envPath, JSON.stringify(process.env));

let buffer = '';
let sessionId = null;
let cwd = process.cwd();
let permissionWaiter = null;
let pendingPrompt = null;
let calls = 0;
let configModel = null;

const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);
const result = (id, value) => send({ jsonrpc: '2.0', id, result: value });
const notify = (method, params) => send({ jsonrpc: '2.0', method, params });

function askPermission(toolCall) {
  return new Promise((resolve) => {
    permissionWaiter = resolve;
    send({
      jsonrpc: '2.0',
      id: 9001,
      method: 'session/request_permission',
      params: {
        sessionId,
        toolCall,
        options: [
          { optionId: 'allow', name: 'Allow once', kind: 'allow_once' },
          { optionId: 'reject', name: 'Reject once', kind: 'reject_once' },
        ],
      },
    });
  });
}

async function prompt(request) {
  const text = (request.params?.prompt ?? []).map((block) => block.text ?? '').join('');
  notify('session/update', { sessionId, update: { sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: 'Thinking about it.' } } });
  for (const chunk of [`You said: `, text, `.`]) {
    notify('session/update', { sessionId, update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: chunk } } });
  }
  const toolCall = { toolCallId: `call-${++calls}`, title: 'Write fixture.txt', kind: 'edit', status: 'pending', locations: [{ path: join(cwd, 'fixture.txt') }], rawInput: { path: 'fixture.txt' } };
  notify('session/update', { sessionId, update: { sessionUpdate: 'tool_call', ...toolCall } });
  const answer = await askPermission(toolCall);
  const outcome = answer?.outcome ?? {};
  if (outcome.outcome === 'selected' && outcome.optionId === 'allow') {
    writeFileSync(join(cwd, 'fixture.txt'), 'written by the fixture agent\n');
    notify('session/update', { sessionId, update: { sessionUpdate: 'tool_call_update', toolCallId: toolCall.toolCallId, status: 'completed', content: [{ type: 'content', content: { type: 'text', text: 'wrote fixture.txt' } }] } });
  } else {
    notify('session/update', { sessionId, update: { sessionUpdate: 'tool_call_update', toolCallId: toolCall.toolCallId, status: 'failed' } });
  }
  if (outcome.outcome === 'cancelled') {
    result(request.id, { stopReason: 'cancelled' });
    return;
  }
  notify('session/update', { sessionId, update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: ' Done.' } } });
  result(request.id, { stopReason: 'end_turn' });
}

function handle(message) {
  if (message.id !== undefined && message.method === undefined) {
    if (message.id === 9001 && permissionWaiter) {
      const resolve = permissionWaiter;
      permissionWaiter = null;
      resolve(message.result);
    }
    return;
  }
  switch (message.method) {
    case 'initialize':
      result(message.id, {
        protocolVersion: 1,
        agentInfo: { name: 'fixture-agent', version: '1.0.0' },
        authMethods: [],
        agentCapabilities: { loadSession: true },
      });
      break;
    case 'session/new':
      sessionId = `fixture-session-${process.pid}`;
      cwd = message.params?.cwd ?? cwd;
      result(message.id, { sessionId });
      break;
    case 'session/load':
      sessionId = message.params?.sessionId ?? null;
      cwd = message.params?.cwd ?? cwd;
      notify('session/update', { sessionId, update: { sessionUpdate: 'user_message_chunk', content: { type: 'text', text: 'replayed history' } } });
      notify('session/update', { sessionId, update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'replayed answer' } } });
      result(message.id, {});
      break;
    case 'session/set_config_option':
      configModel = message.params?.value ?? null;
      result(message.id, { configOptions: [] });
      break;
    case 'session/prompt':
      if (HANG) {
        pendingPrompt = message;
        break;
      }
      void prompt(message);
      break;
    case 'session/cancel':
      if (pendingPrompt) {
        result(pendingPrompt.id, { stopReason: 'cancelled' });
        pendingPrompt = null;
      }
      break;
    default:
      if (message.id !== undefined) {
        send({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: `no method ${message.method}` } });
      }
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
      process.stderr.write(`fixture: bad line: ${String(error)}\n`);
    }
  }
});
process.stdin.on('end', () => process.exit(0));
