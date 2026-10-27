import { createHash } from "node:crypto";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import {
  admitTurn, decideApproval, parseQuestionAnswers, questionText, readSse, splitMessage
} from "../../bridge-core/src/lib.mjs";

const digest = (value) => createHash("sha256").update(JSON.stringify(value)).digest("hex");
const list = (value) => new Set(String(value || "").split(/[\s,]+/u).filter(Boolean));
const bool = (value, name) => {
  if (value === undefined || value === "false") return false;
  if (value === "true") return true;
  throw new Error(`${name} must be true or false`);
};
const required = (env, name) => {
  if (!env[name]?.trim()) throw new Error(`${name} is required`);
  return env[name].trim();
};

export function readConfig(env = process.env) {
  const runtime = new URL(required(env, "CODEWHALE_RUNTIME_URL"));
  if (runtime.username || runtime.password || runtime.search || runtime.hash ||
      (runtime.protocol !== "https:" && !(runtime.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(runtime.hostname)))) {
    throw new Error("Runtime URL requires HTTPS or loopback HTTP without credentials, query or fragment");
  }
  const config = {
    clientId: required(env, "DINGTALK_CLIENT_ID"), clientSecret: required(env, "DINGTALK_CLIENT_SECRET"),
    tenants: list(required(env, "DINGTALK_TENANT_ALLOWLIST")), actors: list(required(env, "DINGTALK_ACTOR_ALLOWLIST")),
    conversations: list(env.DINGTALK_CONVERSATION_ALLOWLIST), allowGroups: bool(env.DINGTALK_ALLOW_GROUPS, "DINGTALK_ALLOW_GROUPS"),
    runtimeUrl: runtime.href.replace(/\/$/u, ""), runtimeToken: required(env, "CODEWHALE_RUNTIME_TOKEN"),
    model: env.CODEWHALE_MODEL?.trim() || "auto", workspace: env.CODEWHALE_WORKSPACE?.trim() || null,
    allowShell: bool(env.CODEWHALE_ALLOW_SHELL, "CODEWHALE_ALLOW_SHELL"),
    statePath: path.resolve(env.DINGTALK_STATE_PATH || ".state/dingtalk.json"), runtimeRequestTimeoutMs: 15000
  };
  if (!config.tenants.size || !config.actors.size || (config.allowGroups && !config.conversations.size)) {
    throw new Error("DingTalk requires nonempty tenant and actor allowlists, and an explicit conversation allowlist for groups");
  }
  return config;
}

export function authorized(config, identity) {
  return Boolean(identity && config.tenants.has(identity.botTenant) && config.tenants.has(identity.senderTenant) &&
    config.actors.has(`${identity.senderTenant}:${identity.staffId}`) &&
    (identity.kind === "1" || (identity.kind === "2" && config.allowGroups && config.conversations.has(`${identity.botTenant}:${identity.conversationId}`))));
}

export function replyRoute(data, now = Date.now()) {
  const url = new URL(data.sessionWebhook);
  if (url.protocol !== "https:" || url.hostname !== "oapi.dingtalk.com" || url.port || url.username || url.password || url.hash ||
      url.pathname !== "/robot/sendBySession" || !url.search || !Number.isSafeInteger(data.sessionWebhookExpiredTime) || data.sessionWebhookExpiredTime <= now) {
    throw new Error("DingTalk reply route is invalid or expired");
  }
  return { url: url.href, expiresAt: data.sessionWebhookExpiredTime };
}

export function incoming(config, frame, now = Date.now()) {
  if (typeof frame?.headers?.messageId !== "string" || typeof frame.data !== "string" || frame.data.length > 65536) return null;
  const data = JSON.parse(frame.data);
  const identity = {
    botTenant: data.chatbotCorpId, senderTenant: data.senderCorpId, staffId: data.senderStaffId,
    conversationId: data.conversationId, kind: data.conversationType
  };
  if (Object.values(identity).some((value) => typeof value !== "string" || !value || value.length > 512) || !authorized(config, identity) ||
      (identity.kind === "2" && data.isInAtList !== true) || data.msgtype !== "text" || typeof data.msgId !== "string" || !data.msgId ||
      typeof data.text?.content !== "string" || !data.text.content.trim() || data.text.content.length > 20000) return null;
  const tuple = [config.clientId, identity.botTenant, identity.conversationId, identity.senderTenant, identity.staffId];
  return {
    chatId: digest(tuple), actorId: `dingtalk:${identity.senderTenant}:${identity.staffId}`, identity,
    messageKey: digest([...tuple, data.msgId]), text: data.text.content.trim(), route: replyRoute(data, now)
  };
}

export function createReplySender({ client, fetchImpl = fetch, now = Date.now }) {
  return async (route, content) => {
    replyRoute({ sessionWebhook: route.url, sessionWebhookExpiredTime: route.expiresAt }, now());
    const token = await client.getAccessToken();
    replyRoute({ sessionWebhook: route.url, sessionWebhookExpiredTime: route.expiresAt }, now());
    const response = await fetchImpl(route.url, {
      method: "POST", redirect: "error", signal: AbortSignal.timeout(15000),
      headers: { "content-type": "application/json", "x-acs-dingtalk-access-token": token },
      body: JSON.stringify({ msgtype: "text", text: { content } })
    });
    const result = await response.json();
    if (!response.ok || result?.errcode !== 0) throw new Error("DingTalk delivery was not confirmed");
  };
}

export class DingTalkBridge {
  constructor({ config, store, runtimeJson, runtimeFetch, sendReply, now = Date.now, report = () => {} }) {
    Object.assign(this, { config, store, runtimeJson, runtimeFetch, sendReply, now, report });
    this.serial = Promise.resolve();
    this.tasks = new Map();
    this.again = new Map();
    this.closed = false;
    this.abort = new AbortController();
    this.capabilities = {};
  }

  mutate(operation) {
    const result = this.serial.then(operation);
    this.serial = result.catch(() => {});
    return result;
  }

  valid(state) {
    return state?.clientId === this.config.clientId && state.runtimeUrl === this.config.runtimeUrl && authorized(this.config, state.identity) &&
      state.actorId === `dingtalk:${state.identity.senderTenant}:${state.identity.staffId}`;
  }

  output(state, id, text) {
    const outbox = state.outbox || [];
    if (outbox.some((entry) => entry.id === id)) return outbox;
    return [...outbox.filter((entry) => entry.status !== "sent").concat(outbox.filter((entry) => entry.status === "sent").slice(-100)),
      { id, chunks: splitMessage(text || "Turn finished without a text response.", 2000), index: 0, status: "pending" }];
  }

  async start() {
    this.capabilities = (await this.runtimeJson("/v1/runtime/info"))?.capabilities || {};
    for (const [chatId] of this.store.listChats()) {
      await this.mutate(async () => {
        const state = await this.store.getChat(chatId);
        if (!this.valid(state)) return;
        let outbox = (state.outbox || []).map((entry) => entry.status === "sending" ? { ...entry, status: "uncertain" } : entry);
        const interrupted = (state.inbox || []).filter((entry) => entry.processing);
        for (const entry of interrupted) outbox = this.output({ outbox }, `${entry.messageKey}:uncertain`, "A previous command may have reached the Engine. Its result is uncertain; it was not repeated. Use /status to inspect the current turn.");
        await this.store.patchChat(chatId, { outbox, inbox: (state.inbox || []).filter((entry) => !entry.processing) });
      });
      this.kick(chatId);
    }
  }

  async accept(frame, acknowledge) {
    let message;
    try { message = incoming(this.config, frame, this.now()); } catch { message = null; }
    if (!message) { acknowledge(); return false; }
    await this.mutate(async () => {
      const state = await this.store.getChat(message.chatId) || {};
      if (state.clientId && !this.valid(state)) throw new Error("DingTalk binding no longer matches current policy");
      const inbox = state.inbox || [];
      if (inbox.some((entry) => entry.messageKey === message.messageKey) || state.pendingAdmission?.messageKey === message.messageKey) { await this.store.save(); return; }
      if (inbox.length >= 100 || (state.outbox || []).filter((entry) => entry.status !== "sent").length >= 100) throw new Error("DingTalk durable queue is full");
      await this.store.recordMessage(message.messageKey, message.chatId, {
        clientId: this.config.clientId, runtimeUrl: this.config.runtimeUrl, identity: message.identity, actorId: message.actorId,
        route: message.route, inbox: [...inbox, { messageKey: message.messageKey, text: message.text }]
      });
      await this.store.save();
    });
    acknowledge();
    this.kick(message.chatId);
    return true;
  }

  spawn(key, operation) {
    if (this.closed) return;
    if (this.tasks.has(key)) { this.again.set(key, operation); return; }
    const task = Promise.resolve().then(operation).catch(() => this.report("DingTalk transport work retained for recovery"))
      .finally(() => {
        this.tasks.delete(key);
        const next = this.again.get(key);
        this.again.delete(key);
        if (next) this.spawn(key, next);
      });
    this.tasks.set(key, task);
  }

  kick(chatId) {
    this.spawn(`inbox:${chatId}`, () => this.drain(chatId));
    this.spawn(`turn:${chatId}`, () => this.runTurn(chatId));
    this.spawn(`output:${chatId}`, () => this.flush(chatId));
  }

  async enqueue(chatId, id, text) {
    await this.mutate(async () => {
      const state = await this.store.getChat(chatId);
      await this.store.patchChat(chatId, { outbox: this.output(state, id, text) });
    });
    this.spawn(`output:${chatId}`, () => this.flush(chatId));
  }

  async drain(chatId) {
    for (;;) {
      const entry = await this.mutate(async () => {
        const state = await this.store.getChat(chatId);
        if (!this.valid(state) || !state.inbox?.length) return null;
        const entry = { ...state.inbox[0], processing: true };
        await this.store.patchChat(chatId, { inbox: [entry, ...state.inbox.slice(1)] });
        return entry;
      });
      if (!entry || this.closed) return;
      try {
        await this.handle(chatId, entry);
      } catch {
        await this.enqueue(chatId, `${entry.messageKey}:error`, "The request could not be confirmed. It will not be repeated automatically. Use /status to inspect the retained state.");
      }
      await this.mutate(async () => {
        const state = await this.store.getChat(chatId);
        await this.store.patchChat(chatId, { inbox: state.inbox.filter((item) => item.messageKey !== entry.messageKey) });
      });
      this.spawn(`turn:${chatId}`, () => this.runTurn(chatId));
    }
  }

  async handle(chatId, entry) {
    const state = await this.store.getChat(chatId);
    const match = /^\/(\w+)(?:\s+([\s\S]*))?$/u.exec(entry.text);
    const command = match?.[1]?.toLowerCase();
    const argument = match?.[2]?.trim() || "";
    const reply = (text) => this.enqueue(chatId, `${entry.messageKey}:reply`, text);
    if (command === "help") return reply("Send a task, /status, /stop, /allow APPROVAL_ID, /deny APPROVAL_ID, or /answer QUESTION_ID followed by your answers. Group conversations require an @mention. Only the initiating human can act on a turn.");
    if (command === "status") return reply(`Thread: ${state.threadId || "not created"}\nTurn: ${state.active?.turnId || "none"}\nAdmission: ${state.pendingAdmission ? "retained; reconciliation pending" : state.threadCreateSubmitted && !state.threadId ? "thread creation uncertain" : "none"}\nUncertain deliveries: ${(state.outbox || []).filter((item) => item.status === "uncertain").length}`);
    if (["allow", "deny"].includes(command)) {
      if (!argument || /\s/u.test(argument)) return reply("Specify one approval ID.");
      await decideApproval(this.runtimeJson, { store: this.store, chatId, actorId: state.actorId, approvalId: argument, decision: command === "allow" ? "allow" : "deny", remember: false });
      return reply("Approval decision recorded.");
    }
    if (["stop", "interrupt"].includes(command)) {
      if (!state.active || this.store.turnOrigin(chatId, state.threadId, state.active.turnId)?.actorId !== state.actorId) return reply("No owned active turn to stop.");
      await this.runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}/turns/${encodeURIComponent(state.active.turnId)}/interrupt`, { method: "POST" });
      return reply("Stop requested.");
    }
    if (command === "answer" || (!command && state.active)) {
      const detail = await this.runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}`);
      const inputId = argument.split(/\s/u)[0];
      const inputs = (detail.pending_user_inputs || []).filter((input) => input.turn_id === state.active?.turnId &&
        this.store.turnOrigin(chatId, state.threadId, input.turn_id)?.actorId === state.actorId);
      const input = command === "answer" ? inputs.find((item) => (item.id || item.input_id) === inputId) : inputs.length === 1 ? inputs[0] : null;
      if (input) {
        const answerText = command === "answer" ? argument.slice(inputId.length).trim() : entry.text;
        const answers = parseQuestionAnswers(input.request, answerText);
        if (!answers) return reply(questionText(input.request));
        await this.runtimeJson(`/v1/user-input/${encodeURIComponent(state.threadId)}/${encodeURIComponent(input.id || input.input_id)}`, { method: "POST", body: { answers } });
        return reply("Answer recorded.");
      }
      if (command === "answer") return reply("That question is not pending for your current turn.");
    }
    if (command) return reply("Unknown command. Use /help.");
    if (state.active || state.pendingAdmission) return reply("Your previous task is still active or awaiting reconciliation. Use /status or /stop.");
    const operationKey = this.capabilities.turn_operation_idempotency === true && this.capabilities.turn_operation_lookup === true ? `dingtalk-${entry.messageKey}` : null;
    const settings = { model: this.config.model, mode: "agent", allow_shell: this.config.allowShell, trust_mode: false, auto_approve: false };
    const pending = {
      messageKey: entry.messageKey, actorId: state.actorId, operationKey, threadId: state.threadId || null, submitted: false, sinceSeq: null,
      threadRequest: { ...settings, workspace: this.config.workspace, archived: false },
      request: { ...settings, prompt: entry.text, input_summary: entry.text, ...(operationKey ? { operation_key: operationKey } : {}) }
    };
    await this.mutate(async () => {
      const current = await this.store.getChat(chatId);
      await this.store.patchChat(chatId, { pendingAdmission: pending, inbox: current.inbox.filter((item) => item.messageKey !== entry.messageKey) });
    });
  }

  async runTurn(chatId) {
    while (!this.closed) {
      let state = await this.store.getChat(chatId);
      if (!this.valid(state)) return;
      try {
        if (state.pendingAdmission) {
          const admission = await admitTurn({
            runtimeJson: this.runtimeJson, pending: state.pendingAdmission, capabilities: this.capabilities,
            persistPending: (pendingAdmission) => this.store.patchChat(chatId, { pendingAdmission }),
            ensureThread: async (body) => {
              const current = await this.store.getChat(chatId);
              if (current.threadId) return current.threadId;
              if (current.threadCreateSubmitted) throw new Error("Thread creation is uncertain");
              await this.store.patchChat(chatId, { threadCreateSubmitted: true });
              const thread = await this.runtimeJson("/v1/threads", { method: "POST", body });
              if (typeof thread?.id !== "string" || !thread.id) throw new Error("Invalid Engine thread receipt");
              await this.store.patchChat(chatId, { threadId: thread.id });
              return thread.id;
            }
          });
          if (!admission.turn) {
            await this.enqueue(chatId, `${state.pendingAdmission.messageKey}:admission`, admission.uncertain ? "Task admission is uncertain and cannot be retried safely. An operator must reconcile the Engine receipt." : "Another turn is running in this Engine thread. Your task is retained until it finishes.");
            if (admission.uncertain) return;
            await delay(3000, undefined, { signal: this.abort.signal });
            continue;
          }
          await this.store.recordTurnOrigin(chatId, admission.pending.threadId, admission.turn.id, admission.pending.actorId);
          await this.store.patchChat(chatId, { pendingAdmission: null, active: { turnId: admission.turn.id, seq: admission.pending.sinceSeq, text: "" } });
          state = await this.store.getChat(chatId);
        }
        if (!state.active) return;
        const response = await this.runtimeFetch(`/v1/threads/${encodeURIComponent(state.threadId)}/events?since_seq=${state.active.seq}`, this.abort.signal);
        if (!response.ok) throw new Error("Engine event stream unavailable");
        for await (const event of readSse(response)) {
          if (this.closed) return;
          let record;
          try { record = JSON.parse(event.data); } catch { continue; }
          await this.consume(chatId, record);
          if (!(await this.store.getChat(chatId)).active) return;
        }
      } catch {
        if (this.closed) return;
        const current = await this.store.getChat(chatId);
        if (current.threadCreateSubmitted && !current.threadId) {
          await this.enqueue(chatId, `${current.pendingAdmission.messageKey}:thread-uncertain`, "Engine thread creation is uncertain and will not be repeated. An operator must reconcile the original Engine request.");
          return;
        }
        this.report("DingTalk Engine connection interrupted; retained receipts will be reconciled");
      }
      await delay(3000, undefined, { signal: this.abort.signal }).catch(() => {});
    }
  }

  async consume(chatId, record) {
    await this.mutate(async () => {
      const state = await this.store.getChat(chatId);
      if (!this.valid(state) || !state.active || !Number.isSafeInteger(record.seq) || record.seq <= state.active.seq) return;
      const active = { ...state.active, seq: record.seq };
      let outbox = state.outbox || [];
      let terminal = false;
      if (record.turn_id === active.turnId) {
        const payload = record.payload || {};
        const id = `${active.turnId}:${record.seq}`;
        if (record.event === "item.delta" && payload.kind === "agent_message" && typeof payload.delta === "string") active.text += payload.delta;
        if (active.text.length > 1000000) active.text = `${active.text.slice(0, 999900)}\n[Response exceeds the bridge text limit; inspect the Engine transcript.]`;
        if (record.event === "approval.required") {
          const approvalId = payload.id || payload.approval_id;
          outbox = this.output({ outbox }, id, `Approval required: ${payload.tool_name || "tool"}\n${payload.description || ""}\n${approvalId ? `/allow ${approvalId}\n/deny ${approvalId}` : "No approval ID was provided; use the Engine client."}`);
        }
        if (record.event === "user_input.required") {
          const inputId = payload.id || payload.input_id;
          outbox = this.output({ outbox }, id, `${questionText(payload.request)}\n${inputId ? `/answer ${inputId} followed by your answers.` : "No question ID was provided; use the Engine client."}`);
        }
        terminal = record.event === "turn.completed" || (record.event === "turn.lifecycle" && ["failed", "canceled", "interrupted"].includes(payload.turn?.status || payload.status));
        if (terminal) outbox = this.output({ outbox }, id, active.text || `Turn ${payload.turn?.status || payload.status || "completed"}.`);
      }
      await this.store.patchChat(chatId, { active: terminal ? null : active, outbox });
    });
    this.spawn(`output:${chatId}`, () => this.flush(chatId));
  }

  async flush(chatId) {
    while (!this.closed) {
      const next = await this.mutate(async () => {
        const state = await this.store.getChat(chatId);
        if (!this.valid(state)) return null;
        const entry = (state.outbox || []).find((item) => item.status === "pending" || item.status === "awaiting_route");
        if (!entry) return null;
        try { replyRoute({ sessionWebhook: state.route?.url, sessionWebhookExpiredTime: state.route?.expiresAt }, this.now()); }
        catch {
          await this.store.patchChat(chatId, { outbox: state.outbox.map((item) => item.id === entry.id ? { ...item, status: "awaiting_route" } : item) });
          return null;
        }
        await this.store.patchChat(chatId, { outbox: state.outbox.map((item) => item.id === entry.id ? { ...item, status: "sending" } : item) });
        return { entry, route: state.route };
      });
      if (!next) return;
      let confirmed = false;
      try { await this.sendReply(next.route, next.entry.chunks[next.entry.index]); confirmed = true; } catch {}
      await this.mutate(async () => {
        const state = await this.store.getChat(chatId);
        await this.store.patchChat(chatId, { outbox: state.outbox.map((item) => item.id !== next.entry.id ? item : {
          ...item, index: confirmed ? item.index + 1 : item.index,
          status: confirmed ? item.index + 1 === item.chunks.length ? "sent" : "pending" : "uncertain"
        }) });
      });
    }
  }

  async stop() {
    this.closed = true;
    this.abort.abort();
    await Promise.allSettled([...this.tasks.values()]);
    await this.serial;
  }
}
