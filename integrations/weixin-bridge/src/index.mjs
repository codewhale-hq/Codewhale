import fs from "node:fs/promises";
import fsSync from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

import {
  getLoginQR,
  waitForLogin,
  getUpdates,
  sendMessage,
  sendTyping,
  getConfig,
  notifyStart,
  notifyStop,
  ILinkLoginBase,
  parseList,
  parseBool,
  envFirst,
  extractText,
  parseWeixinCommand,
  commandAction,
  preservedChatStateFields,
  splitMessage,
  compactRuntimeError,
  latestRunningTurn,
  helpText,
  processUpdateBatch,
  isContinueReply,
  isSessionExpired,
  mintCode,
  parseApprovalReply,
  approvalText,
  questionText,
  parseQuestionAnswers,
} from "./lib.mjs";
import { renderQrToText } from "./qr.mjs";
import { ThreadStore as CoreThreadStore, readJsonSafe, readThreadEvents, writeFileDurable, ApprovalOwnershipError, decideApproval as decideRuntimeApproval } from "../../bridge-core/src/lib.mjs";

// ============================================================================
// 账号持久化
// ============================================================================

function resolveAccountPath(stateDir) {
  return path.join(stateDir, "account.json");
}

async function loadAccount(stateDir) {
  const p = resolveAccountPath(stateDir);
  try {
    const raw = await fs.readFile(p, "utf8");
    return JSON.parse(raw);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    return null;
  }
}

async function saveAccount(stateDir, account) {
  const p = resolveAccountPath(stateDir);
  await fs.mkdir(path.dirname(p), { recursive: true, mode: 0o700 });
  await writeFileDurable(p, `${JSON.stringify(account, null, 2)}\n`, { mode: 0o600 });
}

// ============================================================================
// 配置
// ============================================================================

function requiredEnv(name) {
  const value = process.env[name];
  if (!value || !value.trim()) {
    console.error(`Missing required env: ${name}`);
    process.exit(1);
  }
  return value.trim();
}

function requiredEnvFirst(...names) {
  const value = envFirst(process.env, ...names);
  if (!value) {
    console.error(`Missing required env: one of ${names.join(", ")}`);
    process.exit(1);
  }
  return value;
}

// WEIXIN_* is the canonical spelling; the historical WEXIN_* typo names are
// still honored as deprecated aliases (each warns once per process).
const warnedEnvAliases = new Set();

function weixinEnv(name) {
  const legacy = `WEXIN_${name.slice("WEIXIN_".length)}`;
  const primary = envFirst(process.env, name);
  if (primary) return primary;
  const fallback = envFirst(process.env, legacy);
  if (fallback && !warnedEnvAliases.has(legacy)) {
    warnedEnvAliases.add(legacy);
    console.warn(`${legacy} is deprecated; rename it to ${name}.`);
  }
  return fallback;
}

const config = {
  runtimeUrl: (
    envFirst(process.env, "CODEWHALE_RUNTIME_URL", "DEEPSEEK_RUNTIME_URL") ||
    "http://127.0.0.1:7878"
  ).replace(/\/+$/, ""),
  runtimeToken: requiredEnvFirst(
    "CODEWHALE_RUNTIME_TOKEN",
    "DEEPSEEK_RUNTIME_TOKEN"
  ),
  workspace:
    envFirst(process.env, "CODEWHALE_WORKSPACE", "DEEPSEEK_WORKSPACE") ||
    process.cwd(),
  model:
    envFirst(process.env, "CODEWHALE_MODEL", "DEEPSEEK_MODEL") || "auto",
  mode:
    envFirst(process.env, "CODEWHALE_MODE", "DEEPSEEK_MODE") || "agent",
  allowShell: parseBool(
    envFirst(
      process.env,
      "CODEWHALE_ALLOW_SHELL",
      "DEEPSEEK_ALLOW_SHELL"
    ),
    true
  ),
  trustMode: parseBool(
    envFirst(
      process.env,
      "CODEWHALE_TRUST_MODE",
      "DEEPSEEK_TRUST_MODE"
    ),
    false
  ),
  autoApprove: parseBool(
    envFirst(
      process.env,
      "CODEWHALE_AUTO_APPROVE",
      "DEEPSEEK_AUTO_APPROVE"
    ),
    false
  ),
  allowlist: parseList(
    weixinEnv("WEIXIN_CHAT_ALLOWLIST") ||
      envFirst(
        process.env,
        "CODEWHALE_CHAT_ALLOWLIST",
        "DEEPSEEK_CHAT_ALLOWLIST"
      )
  ),
  allowUnlisted: parseBool(
    weixinEnv("WEIXIN_ALLOW_UNLISTED") ||
      envFirst(
        process.env,
        "CODEWHALE_ALLOW_UNLISTED",
        "DEEPSEEK_ALLOW_UNLISTED"
      ),
    false
  ),
  stateDir:
    weixinEnv("WEIXIN_STATE_DIR") ||
    "/var/lib/codewhale-weixin-bot-bridge",
  // Defaults inside stateDir rather than to a second absolute path: setting
  // only WEIXIN_STATE_DIR must not leave the thread map pointing at /var/lib,
  // which fails with EACCES on every incoming message and silently drops it.
  threadMapPath:
    weixinEnv("WEIXIN_THREAD_MAP_PATH") ||
    path.join(
      weixinEnv("WEIXIN_STATE_DIR") || "/var/lib/codewhale-weixin-bot-bridge",
      "thread-map.json"
    ),
  maxReplyChars: Number(weixinEnv("WEIXIN_MAX_REPLY_CHARS") || 3500),
  maxSendsPerMessage: Math.max(2, Number(weixinEnv("WEIXIN_MAX_SENDS_PER_MESSAGE") || 8)),
  longPollTimeoutMs: Number(
    weixinEnv("WEIXIN_LONGPOLL_TIMEOUT_MS") || 35000
  ),
  turnTimeoutMs: Number(
    envFirst(
      process.env,
      "CODEWHALE_TURN_TIMEOUT_MS",
      "DEEPSEEK_TURN_TIMEOUT_MS"
    ) || 900000
  ),
};

// ============================================================================
// Runtime API 工具
// ============================================================================

function authHeaders() {
  return {
    Authorization: `Bearer ${config.runtimeToken}`,
    "Content-Type": "application/json",
  };
}

async function runtimeJson(subPath, { method = "GET", body = null, auth = true } = {}) {
  const url = `${config.runtimeUrl}${subPath}`;
  const options = { method, headers: auth ? authHeaders() : {}, signal: AbortSignal.timeout(10000) };
  if (body) options.body = JSON.stringify(body);
  const response = await fetch(url, options);
  const result = await readJsonSafe(response, { jsonOnly: true });
  if (!response.ok) {
    const error = new Error(compactRuntimeError(response.status, result));
    error.status = response.status;
    throw error;
  }
  return result;
}

// ============================================================================
// 消息发送 — 通过 iLink sendMessage
// ============================================================================

const WEIXIN_SYSTEM_PROMPT =
  "You are being controlled from WeChat on a phone through iLink Bot. Keep answers short and plain: no wide tables, and long code only when asked. Ask for tool approvals when needed; do not assume mobile messages imply blanket approval.";
const REPLY_WINDOW_MS = 23.5 * 60 * 60 * 1000;
const SEND_GAP_MS = 1000;
const SESSION_PAUSE_MS = 60 * 60 * 1000;
const RATE_LIMIT_BACKOFF_MS = { first: 60_000, max: 15 * 60_000 };
const APPROVAL_CODE_TTL_MS = 30 * 60 * 1000;
const SETTLED_OUTPUTS = new Set(["accepted", "unconfirmed", "skipped"]);

let lastSendAt = 0;
let sessionPausedUntil = 0;

function sessionPaused() {
  return Date.now() < sessionPausedUntil;
}

function pauseSession(source) {
  sessionPausedUntil = Date.now() + SESSION_PAUSE_MS;
  console.error(`iLink reported an expired WeChat session (errcode -14, ${source}). Pausing all iLink calls until ${new Date(sessionPausedUntil).toISOString()}; if it does not recover, delete account.json and scan a new QR code.`);
}

function replyWindow(state) {
  const since = Date.parse(state?.replyWindow?.since || "");
  return { since: Number.isFinite(since) ? since : null, sends: Number(state?.replyWindow?.sends) || 0 };
}

function sendAllowance(state) {
  const { since, sends } = replyWindow(state);
  if (since !== null && Date.now() - since > REPLY_WINDOW_MS) return 0;
  return Math.max(0, config.maxSendsPerMessage - sends);
}

async function deliverChunk(chatId, text, { clientId, contextToken } = {}) {
  if (!botAccount) throw new Error("Bot account unavailable");
  if (sessionPaused()) throw Object.assign(new Error("The WeChat session is paused"), { deliveryStatus: "paused" });
  const state = await threadStore.getChat(chatId);
  if (sendAllowance(state) <= 0) throw Object.assign(new Error("WeChat's reply window or per-message budget is used up"), { deliveryStatus: "held" });
  const { since, sends } = replyWindow(state);
  await threadStore.patchChat(chatId, { replyWindow: { since: new Date(since ?? Date.now()).toISOString(), sends: sends + 1 } });
  const wait = lastSendAt + SEND_GAP_MS - Date.now();
  if (wait > 0) await sleep(wait);
  lastSendAt = Date.now();
  try {
    return await sendMessage({
      baseUrl: botAccount.baseUrl,
      token: botAccount.token,
      body: {
        msg: {
          to_user_id: chatId,
          client_id: clientId || crypto.randomUUID(),
          message_type: 2, // BOT
          message_state: 2, // FINISH
          item_list: [{ type: 1, text_item: { text } }],
          context_token: contextToken ?? (await getContextToken(chatId)),
        },
      },
    });
  } catch (error) {
    if (error.sessionExpired) pauseSession("sendmessage");
    throw error;
  }
}

async function sendText(chatId, text, { clientId, contextToken } = {}) {
  const chunks = splitMessage(text, config.maxReplyChars);
  try {
    for (const [index, chunk] of chunks.entries()) {
      await deliverChunk(chatId, chunk, { clientId: clientId ? `${clientId}-${index}` : undefined, contextToken });
    }
  } catch (error) {
    if (!["held", "paused", "rejected"].includes(error.deliveryStatus)) throw error;
    console.warn(`WeChat reply not sent (${error.deliveryStatus}): ${error.message}`);
  }
}

async function getContextToken(chatId) {
  const state = await threadStore.getChat(chatId);
  // Legacy tokens without an account identity are ambiguous; the next inbound
  // message stores a fresh token bound to the active bot account.
  return state?.contextTokenAccountId === accountIdentity()
    ? state.contextToken || undefined
    : undefined;
}

// ============================================================================
// 命令处理（与 feishu/telegram/wechat bridge 一致）
// ============================================================================

async function handleCommand(chatId, command, inbound = {}) {
  const action = commandAction(command);
  switch (action.kind) {
    case "help":
      await sendText(chatId, helpText());
      return;
    case "status":
      await sendStatus(chatId);
      return;
    case "threads":
      await sendThreads(chatId);
      return;
    case "new_thread": {
      try {
        const state = await ensureThread(chatId, { forceNew: true });
        if (inbound.contextToken) {
          await threadStore.patchChat(chatId, {
            contextToken: inbound.contextToken,
            contextTokenAccountId: accountIdentity(),
          });
        }
        await sendText(chatId, `Created thread ${state.threadId}`);
      } catch (error) {
        await sendText(chatId, `New conversation failed: ${error.message}`);
      }
      return;
    }
    case "resume":
      await resumeThread(chatId, action.threadId);
      return;
    case "interrupt":
      await interruptActiveTurn(chatId);
      return;
    case "compact":
      await compactThread(chatId);
      return;
    case "approval":
      await decideApproval(chatId, action);
      return;
    case "set_model":
      await setChatModel(chatId, action.modelName);
      return;
    case "prompt":
      await runPrompt(chatId, action.prompt, inbound);
      return;
    default:
      await sendText(chatId, helpText());
  }
}

async function ensureThread(chatId, { forceNew = false, threadRequest } = {}) {
  let existing = await threadStore.getChat(chatId);
  const previousAccounts = new Set([existing?.bindingAccountId, existing?.pendingAdmission?.accountId, existing?.turnDelivery?.accountId].filter(Boolean));
  const replacingAccount = forceNew && previousAccounts.size === 1 && !previousAccounts.has(accountIdentity());
  if (forceNew && hasPendingWork(existing) && !replacingAccount) throw new Error("This chat still has a pending turn or retained reply. Use /status first.");
  if (existing?.threadId && !forceNew) {
    if (existing.bindingAccountId !== accountIdentity()) throw new Error("This thread belongs to another or unverified bot account. Use /new to create an explicit binding.");
    return existing;
  }

  if (replacingAccount) {
    // Explicit /new retires the previous account's private receipt before any
    // new side effect. It remains in this store, outside automatic recovery.
    const { retiredAccountStates = {}, ...retired } = existing;
    const receipt = crypto.createHash("sha256").update(JSON.stringify(retired)).digest("hex");
    const retained = { ...retiredAccountStates, [receipt]: retired };
    // Fail before creating a new thread; never evict an uncertain submission
    // or reply merely to make room. Existing over-limit stores remain intact.
    if (Object.keys(retained).length > threadStore.options.actionLimit || Buffer.byteLength(JSON.stringify(retained), "utf8") > 1024 * 1024) {
      throw new Error("Retired account receipt storage is full. Ask the local operator to stop the bridge, back up thread-map.json, and reconcile retiredAccountStates before retrying /new.");
    }
    await threadStore.patchChat(chatId, { retiredAccountStates: retained });
    existing = await threadStore.getChat(chatId);
  }

  const effectiveModel = existing?.model || config.model;

  const thread = await runtimeJson("/v1/threads", {
    method: "POST",
    body: threadRequest || {
      model: effectiveModel,
      workspace: config.workspace,
      mode: config.mode,
      allow_shell: config.allowShell,
      trust_mode: config.trustMode,
      auto_approve: config.autoApprove,
      archived: false,
      system_prompt: WEIXIN_SYSTEM_PROMPT,
    },
  });

  const state = {
    ...preservedChatStateFields(existing, replacingAccount
      ? ["model", "retiredAccountStates"]
      : ["model", "authorizedIdentity", "contextToken", "contextTokenAccountId", "pendingAdmission", "retiredAccountStates"]),
    threadBindings: [...threadBindings(chatId, existing), { threadId: thread.id, accountId: accountIdentity(), actorId: turnActor(chatId) }].slice(-threadStore.options.actionLimit),
    threadId: thread.id,
    bindingAccountId: accountIdentity(),
    lastSeq: 0,
    activeTurnId: null,
    updatedAt: new Date().toISOString(),
  };
  await threadStore.setChat(chatId, state);
  return state;
}

function accountIdentity() {
  return typeof botAccount?.accountId === "string" ? botAccount.accountId.trim() : "";
}

function turnActor(chatId) {
  return accountIdentity() && chatId ? `weixin-account:${accountIdentity()}:user:${chatId}` : "";
}

function threadBindings(chatId, state) {
  const bindings = (Array.isArray(state?.threadBindings) ? state.threadBindings : []).filter((entry) => entry?.threadId && entry.accountId && entry.actorId);
  if (state?.threadId && state.bindingAccountId) {
    const current = { threadId: state.threadId, accountId: state.bindingAccountId, actorId: `weixin-account:${state.bindingAccountId}:user:${chatId}` };
    if (!bindings.some((entry) => entry.threadId === current.threadId && entry.accountId === current.accountId && entry.actorId === current.actorId)) bindings.push(current);
  }
  return bindings.slice(-threadStore.options.actionLimit);
}

function ownsThread(chatId, state, threadId) {
  return threadBindings(chatId, state).some((entry) => entry.threadId === threadId && entry.accountId === accountIdentity() && entry.actorId === turnActor(chatId));
}

function inboundKey(msg) {
  const id = msg.message_id;
  if (!accountIdentity() || !msg.from_user_id || id === undefined || id === null || String(id).trim() === "") return null;
  return JSON.stringify([accountIdentity(), msg.from_user_id, String(id)]);
}

function hasPendingWork(state) {
  return Boolean(state?.pendingAdmission || state?.activeTurnId || state?.turnDelivery?.outputs?.some((entry) => !SETTLED_OUTPUTS.has(entry.status)));
}

function ownsRecovery(chatId, state) {
  const pending = state?.pendingAdmission || state?.turnDelivery;
  return pending?.accountId === accountIdentity() && pending?.actorId === turnActor(chatId) && isAllowed(chatId);
}

function supportsAdmissionRecovery() {
  return runtimeCapabilities.turn_operation_idempotency === true && runtimeCapabilities.turn_operation_lookup === true;
}

async function runPrompt(chatId, prompt, { messageKey } = {}) {
  if (!prompt.trim()) return sendText(chatId, helpText());
  const current = await threadStore.getChat(chatId);
  if (hasPendingWork(current)) {
    startChatRecovery(chatId);
    return sendText(chatId, "This chat has a pending turn or retained reply. Use /status or /interrupt.");
  }
  if (current?.threadId && current.bindingAccountId !== accountIdentity()) return sendText(chatId, "This conversation belongs to another or unverified bot account. Use /new to create a new conversation.");
  if (!messageKey || !turnActor(chatId)) throw new Error("A stable incoming message identity is required.");
  const request = {
    prompt, input_summary: prompt.slice(0, 200), model: current?.model || config.model,
    mode: config.mode, allow_shell: config.allowShell, trust_mode: config.trustMode, auto_approve: config.autoApprove,
  };
  // Retain the frozen request before any admission side effect or poll acknowledgement.
  const operationKey = supportsAdmissionRecovery() ? crypto.createHash("sha256").update(messageKey).digest("hex") : null;
  if (operationKey) request.operation_key = operationKey;
  await threadStore.patchChat(chatId, { pendingAdmission: {
    accountId: accountIdentity(), actorId: turnActor(chatId), messageKey, request, operationKey,
    threadId: current?.threadId || null, submitted: false, sinceSeq: null,
    threadRequest: { model: request.model, workspace: config.workspace, mode: request.mode, allow_shell: request.allow_shell,
      trust_mode: request.trust_mode, auto_approve: request.auto_approve, archived: false,
      system_prompt: WEIXIN_SYSTEM_PROMPT },
  } });
  startChatRecovery(chatId);
}

async function reconcileAdmission(chatId) {
  let state = await threadStore.getChat(chatId);
  let pending = state?.pendingAdmission;
  if (!pending || !ownsRecovery(chatId, state)) return;
  if (!pending.threadId) {
    const thread = await ensureThread(chatId, { threadRequest: pending.threadRequest });
    // ensureThread replaces chat state only for a new thread; retain the admission.
    pending = { ...pending, threadId: thread.threadId };
    await threadStore.patchChat(chatId, { pendingAdmission: pending });
  }
  if (pending.sinceSeq === null) {
    const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(pending.threadId)}`);
    if (latestRunningTurn(detail)) {
      await threadStore.patchChat(chatId, { pendingAdmission: { ...pending, blocked: true } });
      return;
    }
    pending = { ...pending, sinceSeq: Number(detail.latest_seq || 0), blocked: false };
    await threadStore.patchChat(chatId, { pendingAdmission: pending });
  }
  let turn;
  if (pending.submitted) {
    if (!pending.operationKey || !supportsAdmissionRecovery()) return;
    try {
      turn = await runtimeJson(`/v1/threads/${encodeURIComponent(pending.threadId)}/turn-operations/${encodeURIComponent(pending.operationKey)}`);
    } catch (error) {
      // 409 is an incomplete durable admission, not permission to create another turn.
      if (error.status !== 404) throw error;
    }
  }
  if (!turn) {
    pending = { ...pending, submitted: true };
    await threadStore.patchChat(chatId, { pendingAdmission: pending });
    const result = await runtimeJson(`/v1/threads/${encodeURIComponent(pending.threadId)}/turns`, { method: "POST", body: pending.request });
    turn = result?.turn;
  }
  if (!turn?.id || turn.thread_id !== pending.threadId) throw new Error("Invalid Engine admission receipt");
  await threadStore.recordTurnOrigin(chatId, pending.threadId, turn.id, pending.actorId);
  await threadStore.patchChat(chatId, {
    pendingAdmission: null, activeTurnId: turn.id, lastSeq: pending.sinceSeq,
    turnDelivery: { accountId: pending.accountId, actorId: pending.actorId, threadId: pending.threadId,
      turnId: turn.id, messageKey: pending.messageKey, nextSeq: pending.sinceSeq, responseText: "", outputs: [], terminal: false },
  });
}

function queueOutput(delivery, key, text) {
  if (!text || delivery.outputs.some((entry) => entry.key === key)) return;
  // Persist one independently acknowledged chunk per API call. A stable client ID
  // helps reconciliation but iLink does not promise exactly-once recipient delivery.
  for (const [index, chunk] of splitMessage(text, config.maxReplyChars).entries()) {
    const chunkKey = `${key}:${index}`;
    if (delivery.outputs.some((entry) => entry.key === chunkKey)) continue;
    delivery.outputs.push({ key: chunkKey, text: chunk, status: "queued",
      clientId: crypto.createHash("sha256").update(`${delivery.accountId}:${delivery.turnId}:${chunkKey}`).digest("hex") });
  }
}

function hasOutput(delivery, key) {
  return delivery.outputs.some((entry) => entry.key.startsWith(`${key}:`));
}

function mintApprovalCode(delivery, approvalId) {
  const codes = { ...(delivery.approvalCodes || {}) };
  let code;
  do code = mintCode(); while (codes[code]);
  codes[code] = { approvalId, expiresAt: new Date(Date.now() + APPROVAL_CODE_TTL_MS).toISOString() };
  delivery.approvalCodes = Object.fromEntries(Object.entries(codes).slice(-100));
  return code;
}

function queueApproval(delivery, approval, fallbackKey) {
  const approvalId = approval.approval_id || approval.id;
  if (!approvalId) {
    queueOutput(delivery, `approval:${fallbackKey}`, "需要你的批准，请在 Codewhale 中处理。/ An approval is waiting; decide it in Codewhale.");
    return;
  }
  const key = `approval:${approvalId}`;
  if (hasOutput(delivery, key)) return;
  queueOutput(delivery, key, approvalText(approval, mintApprovalCode(delivery, approvalId)));
}

function queueQuestion(delivery, input) {
  const inputId = input?.input_id || input?.id;
  if (!inputId || !input.request) return;
  delivery.pendingQuestion = { inputId, request: input.request };
  queueOutput(delivery, `question:${inputId}`, questionText(input.request));
}

function applyTurnEvent(delivery, record, seq) {
  const payload = record.payload || {};
  if (record.event === "item.delta" && payload.kind === "agent_message") delivery.responseText += payload.delta || "";
  if (record.event === "approval.required") queueApproval(delivery, payload, seq);
  if (record.event === "user_input.required") queueQuestion(delivery, payload);
  if ((record.event === "user_input.answered" || record.event === "user_input.canceled")
    && delivery.pendingQuestion?.inputId === (payload.input_id || payload.id)) delivery.pendingQuestion = null;
  const status = payload.turn?.status || payload.status;
  if (record.event === "turn.completed") finishDelivery(delivery, status || "completed");
  if (record.event === "turn.lifecycle" && ["failed", "canceled", "interrupted"].includes(status)) finishDelivery(delivery, status);
}

function finishDelivery(delivery, status) {
  if (delivery.terminal) return;
  delivery.terminal = true;
  delivery.status = status;
  delivery.pendingQuestion = null;
  const text = delivery.responseText.trim();
  queueOutput(delivery, "result", [text, status !== "completed" ? `Turn ${status}.` : (!text ? "Turn completed." : "")].filter(Boolean).join("\n\n"));
}

async function saveDelivery(chatId, delivery) {
  await threadStore.patchChat(chatId, { turnDelivery: delivery, lastSeq: delivery.nextSeq });
}

async function updateDelivery(chatId, change) {
  const state = await threadStore.getChat(chatId);
  if (!state?.turnDelivery) return null;
  const delivery = structuredClone(state.turnDelivery);
  if (change(delivery)) await saveDelivery(chatId, delivery);
  return delivery;
}

function holdQueued(delivery, fromIndex) {
  for (const entry of delivery.outputs.slice(fromIndex)) if (entry.status === "queued") entry.status = "held";
}

function insertContinueNotice(delivery, index) {
  const remaining = delivery.outputs.slice(index).filter((entry) => entry.status === "queued").length;
  holdQueued(delivery, index);
  const key = `continue:${crypto.randomUUID()}`;
  delivery.outputs.splice(index, 0, { key: `${key}:0`, status: "queued",
    text: `（还有 ${remaining} 条，回复「继续」查看 / Reply 继续 for ${remaining} more）`,
    clientId: crypto.createHash("sha256").update(`${delivery.accountId}:${delivery.turnId}:${key}`).digest("hex") });
}

async function flushOutputs(chatId) {
  for (;;) {
    const state = await threadStore.getChat(chatId);
    if (!state?.turnDelivery || !ownsRecovery(chatId, state)) return;
    if (threadStore.turnOrigin(chatId, state.turnDelivery.threadId, state.turnDelivery.turnId)?.actorId !== state.turnDelivery.actorId) return;
    if (sessionPaused()) return;
    const allowance = sendAllowance(state);
    let target = null;
    const delivery = await updateDelivery(chatId, (draft) => {
      let changed = false;
      for (const entry of draft.outputs) if (entry.status === "sending") { entry.status = "uncertain"; changed = true; }
      if (draft.releaseHeld) {
        for (const entry of draft.outputs) if (entry.status === "held") entry.status = "queued";
        draft.releaseHeld = false;
        changed = true;
      }
      const index = draft.outputs.findIndex((entry) => entry.status === "queued");
      if (index < 0 || Date.parse(draft.outputs[index].retryAt || "") > Date.now()) return changed;
      if (allowance <= 0) {
        holdQueued(draft, index);
        return true;
      }
      if (allowance === 1 && draft.outputs.slice(index + 1).some((entry) => entry.status === "queued")) insertContinueNotice(draft, index);
      target = draft.outputs[index];
      target.status = "sending";
      return true;
    });
    if (!target) {
      if (delivery?.terminal && delivery.outputs.every((entry) => SETTLED_OUTPUTS.has(entry.status))) {
        await threadStore.patchChat(chatId, { activeTurnId: null });
      }
      return;
    }
    let outcome;
    try {
      const response = await deliverChunk(chatId, target.text, { clientId: `${target.clientId}-0` });
      outcome = response?.message_id ? { status: "accepted", serverMessageId: String(response.message_id) } : { status: "unconfirmed" };
      if (!response?.message_id) console.warn("WeChat accepted a reply without a message_id; Tencent may be filtering this account's messages.");
    } catch (error) {
      if (error.deliveryStatus === "paused" || error.sessionExpired) outcome = { status: "queued" };
      else if (error.rateLimited) outcome = { status: "queued", rateLimited: true };
      else if (error.deliveryStatus === "held" || error.deliveryStatus === "rejected") outcome = { status: "held", holdRest: true };
      else outcome = { status: "uncertain" };
    }
    await updateDelivery(chatId, (draft) => {
      const index = draft.outputs.findIndex((entry) => entry.key === target.key);
      if (index < 0) return false;
      const entry = draft.outputs[index];
      entry.status = outcome.status;
      if (outcome.serverMessageId) entry.serverMessageId = outcome.serverMessageId;
      if (outcome.rateLimited) {
        draft.rateLimitBackoffMs = Math.min(draft.rateLimitBackoffMs ? draft.rateLimitBackoffMs * 2 : RATE_LIMIT_BACKOFF_MS.first, RATE_LIMIT_BACKOFF_MS.max);
        entry.retryAt = new Date(Date.now() + draft.rateLimitBackoffMs).toISOString();
      } else if (outcome.status !== "queued") {
        draft.rateLimitBackoffMs = 0;
        delete entry.retryAt;
      }
      if (outcome.holdRest) holdQueued(draft, index);
      return true;
    });
    if (outcome.status !== "accepted" && outcome.status !== "unconfirmed") return;
  }
}

function queuedWaitMs(delivery) {
  const queued = delivery.outputs.filter((entry) => entry.status === "queued");
  if (!queued.length) return null;
  const retryAt = Math.min(...queued.map((entry) => Date.parse(entry.retryAt || "") || 0));
  return Math.max(1000, sessionPausedUntil - Date.now(), retryAt - Date.now());
}

async function reconcileSnapshot(chatId) {
  const state = await threadStore.getChat(chatId);
  if (!state?.turnDelivery || !ownsRecovery(chatId, state)) return;
  const { threadId, turnId } = state.turnDelivery;
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(threadId)}`);
  const turn = detail.turns?.find((entry) => entry.id === turnId && entry.thread_id === threadId);
  if (!turn) throw new Error("Accepted Engine turn is unavailable");
  const items = (detail.items || []).filter((item) => item.turn_id === turnId && item.kind === "agent_message");
  if (items.some((item) => typeof item.detail !== "string")) throw new Error("Engine snapshot lacks full assistant output");
  const inputs = (detail.pending_user_inputs || []).filter((input) => input.turn_id === turnId);
  await updateDelivery(chatId, (delivery) => {
    if (delivery.turnId !== turnId) return false;
    if (items.length) delivery.responseText = items.map((item) => item.detail).join("\n\n");
    delivery.nextSeq = Math.max(delivery.nextSeq, Number(detail.latest_seq || 0));
    for (const approval of detail.pending_approvals || []) {
      if (approval.turn_id === turnId) queueApproval(delivery, approval, approval.id);
    }
    for (const input of inputs) queueQuestion(delivery, input);
    if (delivery.pendingQuestion && !inputs.some((input) => input.id === delivery.pendingQuestion.inputId)) delivery.pendingQuestion = null;
    if (["completed", "failed", "canceled", "interrupted"].includes(turn.status)) finishDelivery(delivery, turn.status);
    return true;
  });
}

const typingStates = new Map();

function startTyping(chatId) {
  if (typingStates.has(chatId) || !botAccount || sessionPaused()) return;
  const typing = { ticket: null, timer: null };
  typingStates.set(chatId, typing);
  const tick = async () => {
    try {
      if (sessionPaused()) return;
      if (!typing.ticket) {
        const accountConfig = await getConfig({ baseUrl: botAccount.baseUrl, token: botAccount.token, ilinkUserId: chatId, contextToken: await getContextToken(chatId) });
        typing.ticket = accountConfig?.typing_ticket || "";
      }
      if (typing.ticket && typingStates.get(chatId) === typing) {
        await sendTyping({ baseUrl: botAccount.baseUrl, token: botAccount.token, ilinkUserId: chatId, typingTicket: typing.ticket, status: 1 });
      }
    } catch {
      if (typingStates.get(chatId) === typing) stopTyping(chatId, false);
    }
  };
  typing.timer = setInterval(tick, 5000);
  typing.timer.unref?.();
  tick();
}

function stopTyping(chatId, cancel = true) {
  const typing = typingStates.get(chatId);
  if (!typing) return;
  clearInterval(typing.timer);
  typingStates.delete(chatId);
  if (cancel && typing.ticket && botAccount && !sessionPaused()) {
    sendTyping({ baseUrl: botAccount.baseUrl, token: botAccount.token, ilinkUserId: chatId, typingTicket: typing.ticket, status: 2 }).catch(() => {});
  }
}

async function streamTurnEvents(chatId, controller) {
  const start = (await threadStore.getChat(chatId)).turnDelivery;
  // Bound headers and streaming together; this never interrupts the Engine turn.
  const timeout = setTimeout(() => controller.abort(), config.turnTimeoutMs);
  try {
    const response = await fetch(`${config.runtimeUrl}/v1/threads/${encodeURIComponent(start.threadId)}/events?since_seq=${start.nextSeq}`,
      { headers: authHeaders(), signal: controller.signal });
    if (!response.ok) throw new Error("Engine event observation unavailable");
    for await (const record of readThreadEvents(response, start.threadId)) {
      if (record.event === "stream.end") {
        if (record.payload?.retryable === false || record.retryable === false) return false;
        break;
      }
      const seq = Number(record.seq || 0);
      // Cursor, accumulator and pending output are committed together before send.
      const delivery = await updateDelivery(chatId, (draft) => {
        if (draft.turnId !== start.turnId || seq <= draft.nextSeq) return false;
        draft.nextSeq = seq;
        if (record.turn_id === draft.turnId) applyTurnEvent(draft, record, seq);
        return true;
      });
      if (!delivery || delivery.turnId !== start.turnId) return true;
      if (record.turn_id === start.turnId) {
        if (record.event === "item.delta") startTyping(chatId);
        if (record.event === "approval.required" || record.event === "user_input.required") stopTyping(chatId);
      }
      await flushOutputs(chatId);
      if (delivery.terminal) return true;
      if (stopping) break;
    }
    return true;
  } finally {
    clearTimeout(timeout);
    controller.abort();
    stopTyping(chatId);
  }
}

function startChatRecovery(chatId) {
  if (stopping || recoveryTasks.has(chatId)) return;
  const task = (async () => {
    while (!stopping) {
      let state = await threadStore.getChat(chatId);
      if (!ownsRecovery(chatId, state)) return;
      try {
        if (state.pendingAdmission) {
          await reconcileAdmission(chatId);
          state = await threadStore.getChat(chatId);
          if (state.pendingAdmission) {
            if (state.pendingAdmission.submitted && !state.pendingAdmission.operationKey) return;
            await sleep(1000);
            continue;
          }
        }
        if (!state.turnDelivery) return;
        if (!state.turnDelivery.terminal) await reconcileSnapshot(chatId);
        await flushOutputs(chatId);
        state = await threadStore.getChat(chatId);
        if (state.turnDelivery?.terminal) {
          const wait = queuedWaitMs(state.turnDelivery);
          if (wait === null) return;
          await sleep(wait);
          continue;
        }
        const controller = new AbortController();
        recoveryControllers.set(chatId, controller);
        const retryable = await streamTurnEvents(chatId, controller);
        recoveryControllers.delete(chatId);
        if (retryable === false) return;
      } catch {
        if (stopping) return;
        console.warn("Turn observation interrupted; retaining the accepted turn for recovery.");
      }
      await sleep(1000);
    }
  })().finally(() => { recoveryTasks.delete(chatId); recoveryControllers.delete(chatId); });
  recoveryTasks.set(chatId, task);
}

async function reissueApprovalCodes(chatId) {
  const state = await threadStore.getChat(chatId);
  const delivery = state?.turnDelivery;
  if (!delivery || delivery.terminal || !ownsRecovery(chatId, state)) return [];
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(delivery.threadId)}`);
  const pending = (detail.pending_approvals || []).filter((approval) => approval.turn_id === delivery.turnId);
  const lines = [];
  await updateDelivery(chatId, (draft) => {
    if (draft.turnId !== delivery.turnId) return false;
    for (const approval of pending) lines.push(approvalText(approval, mintApprovalCode(draft, approval.approval_id || approval.id)));
    return lines.length > 0;
  });
  return lines;
}

async function decideByCode(chatId, { decision, code }) {
  const state = await threadStore.getChat(chatId);
  const entry = ownsRecovery(chatId, state) ? state?.turnDelivery?.approvalCodes?.[code] : null;
  if (!entry || entry.usedAt) return sendText(chatId, "没有找到等待中的这个批准码。/ No approval is waiting with that code.");
  if (Date.parse(entry.expiresAt) <= Date.now()) return sendText(chatId, "这个批准码已过期，发送 /status 获取新的。/ That code expired; send /status for a new one.");
  try {
    await decideRuntimeApproval(runtimeJson, { store: threadStore, chatId, actorId: turnActor(chatId), approvalId: entry.approvalId, decision, remember: false });
  } catch (error) {
    if (error instanceof ApprovalOwnershipError || error.status === 404) return sendText(chatId, "这个批准已不在等待中。/ That approval is no longer waiting.");
    return sendText(chatId, `Approval failed: ${error.message}`);
  }
  await updateDelivery(chatId, (draft) => {
    if (!draft.approvalCodes?.[code]) return false;
    draft.approvalCodes[code].usedAt = new Date().toISOString();
    return true;
  });
  await sendText(chatId, decision === "allow" ? "已允许 / Allowed" : "已拒绝 / Denied");
}

async function answerQuestion(chatId, text) {
  const state = await threadStore.getChat(chatId);
  const delivery = state?.turnDelivery;
  const question = delivery?.pendingQuestion;
  const answers = parseQuestionAnswers(question?.request, text);
  if (!answers) return sendText(chatId, "没能对上这个回答，请回复选项编号或文字。/ That did not match; reply with an option number or in words.");
  try {
    await runtimeJson(`/v1/user-input/${encodeURIComponent(delivery.threadId)}/${encodeURIComponent(question.inputId)}`, { method: "POST", body: { answers } });
  } catch (error) {
    if (error.status !== 404) return sendText(chatId, `Answer failed: ${error.message}`);
    await updateDelivery(chatId, (draft) => {
      if (draft.pendingQuestion?.inputId !== question.inputId) return false;
      draft.pendingQuestion = null;
      return true;
    });
    return sendText(chatId, "这个问题已不在等待中。/ That question is no longer waiting.");
  }
  await updateDelivery(chatId, (draft) => {
    if (draft.pendingQuestion?.inputId !== question.inputId) return false;
    draft.pendingQuestion = null;
    return true;
  });
  await sendText(chatId, "已回答 / Answer sent");
}

async function sendStatus(chatId) {
  try {
    const state = await threadStore.getChat(chatId);
    const [health, runtimeInfo, workspace] = await Promise.all([
      runtimeJson("/health", { auth: false }),
      runtimeJson("/v1/runtime/info"),
      runtimeJson("/v1/workspace/status"),
    ]);
    const outputs = ownsRecovery(chatId, state) ? state?.turnDelivery?.outputs || [] : [];
    const held = outputs.filter((entry) => entry.status === "held").length;
    const unconfirmed = outputs.filter((entry) => entry.status === "unconfirmed").length;
    const approvals = await reissueApprovalCodes(chatId);
    await sendText(
      chatId,
      [
        `user_id=${chatId}`,
        state?.pendingAdmission ? "Turn admission is pending; its original request is retained." : "",
        state?.activeTurnId && ownsRecovery(chatId, state) ? `accepted_turn=${state.activeTurnId}` : "",
        outputs.some((entry) => entry.status === "uncertain") ? "A reply has unconfirmed API acceptance. It is retained for review and will not be resent automatically." : "",
        held ? `还有 ${held} 条回复未发送，回复「继续」查看。/ ${held} replies are waiting; reply 继续.` : "",
        unconfirmed ? `微信接受了 ${unconfirmed} 条回复但没有返回消息 ID，可能没有显示。/ WeChat accepted ${unconfirmed} replies without a message ID; they may not have been shown.` : "",
        ownsRecovery(chatId, state) && state?.turnDelivery?.pendingQuestion ? "Codewhale 正在等你回答上面的问题。/ Codewhale is waiting for your answer to its question." : "",
        ...approvals,
        `runtime=${health.status || "unknown"}`,
        `version=${runtimeInfo.version || "unknown"}`,
        `bind=${runtimeInfo.bind_host}:${runtimeInfo.port}`,
        `auth_required=${runtimeInfo.auth_required}`,
        `workspace=${workspace.workspace}`,
        `git_repo=${workspace.git_repo}`,
        workspace.branch ? `branch=${workspace.branch}` : "",
        `staged=${workspace.staged} unstaged=${workspace.unstaged} untracked=${workspace.untracked}`,
      ]
        .filter(Boolean)
        .join("\n")
    );
  } catch (error) {
    await sendText(chatId, `Status check failed: ${error.message}`);
  }
}

async function sendThreads(chatId) {
  try {
    const state = await threadStore.getChat(chatId);
    const ownedIds = threadBindings(chatId, state)
      .filter((entry) => entry.accountId === accountIdentity() && entry.actorId === turnActor(chatId))
      .map((entry) => entry.threadId);
    if (!ownedIds.length) {
      await sendText(chatId, "No recent runtime threads owned by this account and chat.");
      return;
    }
    const query = new URLSearchParams({ limit: "8", include_archived: "true", thread_ids: ownedIds.join(",") });
    const threads = (await runtimeJson(`/v1/threads/summary?${query}`))
      .filter((thread) => ownsThread(chatId, state, thread.id));
    if (!threads.length) {
      await sendText(chatId, "No recent runtime threads owned by this account and chat.");
      return;
    }
    await sendText(
      chatId,
      threads
        .map((thread) => {
          const status = thread.latest_turn_status || "none";
          return `${thread.id} [${status}] ${thread.title || thread.preview || ""}`;
        })
        .join("\n")
    );
  } catch (error) {
    await sendText(chatId, `Thread listing failed: ${error.message}`);
  }
}

async function resumeThread(chatId, args) {
  const threadId = args.trim();
  if (!threadId) {
    await sendText(chatId, "Usage: /resume <thread_id>");
    return;
  }
  try {
    const existing = await threadStore.getChat(chatId);
    if (hasPendingWork(existing)) throw new Error("This chat still has a pending turn or retained reply. Use /status first.");
    if (!ownsThread(chatId, existing, threadId)) throw new ApprovalOwnershipError("Thread is not owned by this account and chat");
    const detail = await runtimeJson(
      `/v1/threads/${encodeURIComponent(threadId)}`
    );
    await threadStore.setChat(chatId, {
      ...preservedChatStateFields(existing, ["model", "authorizedIdentity", "contextToken", "contextTokenAccountId", "retiredAccountStates"]),
      threadBindings: threadBindings(chatId, existing),
      threadId,
      bindingAccountId: accountIdentity(),
      lastSeq: Number(detail.latest_seq || 0),
      activeTurnId: null,
      updatedAt: new Date().toISOString(),
    });
    await sendText(chatId, `Resumed thread ${threadId}`);
  } catch (error) {
    await sendText(chatId, `Resume failed: ${error.message}`);
  }
}

async function interruptActiveTurn(chatId) {
  const state = await threadStore.getChat(chatId);
  if (!state?.threadId) {
    await sendText(chatId, "No runtime thread recorded for this chat.");
    return;
  }
  try {
    const detail = await runtimeJson(
      `/v1/threads/${encodeURIComponent(state.threadId)}`
    );
    const turnId = state.activeTurnId;
    if (!turnId) {
      await sendText(chatId, "No active turn recorded for this chat.");
      return;
    }
    if (state.turnDelivery?.accountId !== accountIdentity() || threadStore.turnOrigin(chatId, state.threadId, turnId)?.actorId !== turnActor(chatId)) throw new ApprovalOwnershipError("Turn is not owned by this account and chat");
    await runtimeJson(
      `/v1/threads/${encodeURIComponent(state.threadId)}/turns/${encodeURIComponent(turnId)}/interrupt`,
      { method: "POST" }
    );
    await threadStore.patchChat(chatId, {
      activeTurnId: turnId,
      updatedAt: new Date().toISOString(),
    });
    await sendText(chatId, `Interrupt requested for ${turnId}`);
  } catch (error) {
    await sendText(chatId, `Interrupt failed: ${error.message}`);
  }
}

async function compactThread(chatId) {
  try {
    const state = await ensureThread(chatId);
    const result = await runtimeJson(
      `/v1/threads/${encodeURIComponent(state.threadId)}/compact`,
      {
        method: "POST",
        body: { reason: "weixin-bot bridge request" },
      }
    );
    await sendText(
      chatId,
      `Compaction started: ${result.turn?.id || "unknown turn"}`
    );
  } catch (error) {
    await sendText(chatId, `Compact failed: ${error.message}`);
  }
}

async function decideApproval(chatId, action) {
  const decision = action.decision;
  const { approvalId, remember } = action;
  if (!approvalId) {
    await sendText(
      chatId,
      `Usage: /${decision} <approval_id>${decision === "allow" ? " [remember]" : ""}`
    );
    return;
  }
  try {
    await decideRuntimeApproval(runtimeJson, { store: threadStore, chatId, actorId: turnActor(chatId), approvalId, decision, remember });
    await sendText(
      chatId,
      `Approval ${approvalId}: ${decision}${remember ? " and remember" : ""}`
    );
  } catch (error) {
    if (error instanceof ApprovalOwnershipError) {
      await sendText(chatId, `Approval ${approvalId} is not waiting in this chat.`);
      return;
    }
    await sendText(chatId, `Approval failed: ${error.message}`);
  }
}

async function setChatModel(chatId, modelName) {
  if (!modelName || modelName === "default") {
    await threadStore.patchChat(chatId, {
      model: null,
      updatedAt: new Date().toISOString(),
    });
    await sendText(
      chatId,
      `Reset per-chat model. Using bridge default: ${config.model}`
    );
    return;
  }
  await threadStore.patchChat(chatId, {
    model: modelName,
    updatedAt: new Date().toISOString(),
  });
  await sendText(chatId, `Per-chat model set to: ${modelName}`);
}

// ============================================================================
// 主循环 — 长轮询 getUpdates
// ============================================================================

let botAccount = null;
let stopping = false;
let threadStore;
const recoveryTasks = new Map();
const recoveryControllers = new Map();
let runtimeCapabilities = {};

function resolveSyncBufPath(stateDir) {
  return path.join(stateDir, "sync-buf.txt");
}

async function loadSyncBuf(stateDir) {
  const p = resolveSyncBufPath(stateDir);
  try {
    return await fs.readFile(p, "utf8");
  } catch {
    return "";
  }
}

async function saveSyncBuf(stateDir, buf) {
  const p = resolveSyncBufPath(stateDir);
  // The state dir may not exist yet on a first run whose first persisted write
  // is the poll cursor rather than account.json.
  await fs.mkdir(path.dirname(p), { recursive: true, mode: 0o700 });
  await writeFileDurable(p, buf, { mode: 0o600 });
}

const refusedSenders = new Set();

async function refuseSender(msg) {
  const fromUser = msg.from_user_id;
  if (!fromUser || !msg.context_token || refusedSenders.has(fromUser) || refusedSenders.size >= 100 || sessionPaused()) return;
  refusedSenders.add(fromUser);
  console.warn(`Refused a chat from an unlisted WeChat user (user_id=${fromUser}).`);
  try {
    await sendMessage({
      baseUrl: botAccount.baseUrl,
      token: botAccount.token,
      body: { msg: { to_user_id: fromUser, client_id: crypto.randomUUID(), message_type: 2, message_state: 2,
        item_list: [{ type: 1, text_item: { text: `此微信未获授权使用这个 Codewhale。/ This WeChat is not allowed to use this Codewhale. (user_id=${fromUser})` } }],
        context_token: msg.context_token } },
    });
  } catch (error) {
    if (error.sessionExpired) pauseSession("sendmessage");
  }
}

async function settleHeldReplies(chatId, text) {
  const state = await threadStore.getChat(chatId);
  const delivery = state?.turnDelivery;
  if (!delivery || !ownsRecovery(chatId, state) || !delivery.outputs.some((entry) => entry.status === "held")) return "none";
  if (isContinueReply(text)) {
    await updateDelivery(chatId, (draft) => { draft.releaseHeld = true; return true; });
    startChatRecovery(chatId);
    return "consumed";
  }
  const command = commandAction(parseWeixinCommand(text));
  if (!delivery.terminal || command.kind !== "prompt" || parseApprovalReply(text)) {
    await updateDelivery(chatId, (draft) => { draft.releaseHeld = true; return true; });
    startChatRecovery(chatId);
    return "released";
  }
  const seen = delivery.outputs.some((entry) => entry.status === "accepted" || entry.status === "unconfirmed");
  if (seen) {
    const settled = await updateDelivery(chatId, (draft) => {
      for (const entry of draft.outputs) if (entry.status === "held") entry.status = "skipped";
      return true;
    });
    if (settled?.outputs.every((entry) => SETTLED_OUTPUTS.has(entry.status))) await threadStore.patchChat(chatId, { activeTurnId: null });
    return "skipped";
  }
  await updateDelivery(chatId, (draft) => {
    const index = draft.outputs.findIndex((entry) => entry.status === "held");
    const key = `waiting:${crypto.randomUUID()}`;
    draft.outputs.splice(index, 0, { key: `${key}:0`, status: "held",
      text: "你有一条之前的回复（下一条）。你刚才的消息还没有发给 Codewhale，看完后请重新发送。/ You have an earlier answer (next message). Your new message was not run yet; send it again after reading.",
      clientId: crypto.createHash("sha256").update(`${draft.accountId}:${draft.turnId}:${key}`).digest("hex") });
    draft.releaseHeld = true;
    return true;
  });
  startChatRecovery(chatId);
  return "consumed";
}

/** One durably claimed inbound message; agent observation runs separately. */
async function handleInbound(msg) {
  const fromUser = msg.from_user_id || "";
  if (msg.message_type !== undefined && msg.message_type !== 1) return;
  if (!isAllowed(fromUser)) return refuseSender(msg);
  await threadStore.patchChat(fromUser, { replyWindow: { since: new Date().toISOString(), sends: 0 } });
  const text = extractText(msg.item_list);
  if (msg.context_token) {
    const state = await threadStore.getChat(fromUser);
    // A pending admission already owns its context before a thread is bound.
    // Preserve foreign or unattributed receipts until /new retires them.
    if ((!state?.bindingAccountId || state.bindingAccountId === accountIdentity())
      && (!state?.pendingAdmission || state.pendingAdmission.accountId === accountIdentity())
      && (!state?.turnDelivery || state.turnDelivery.accountId === accountIdentity())) {
      await threadStore.patchChat(fromUser, {
        contextToken: msg.context_token,
        contextTokenAccountId: accountIdentity(),
      });
    }
  }
  if (!text) return sendText(fromUser, "仅支持文本和语音消息。图片/视频/文件暂不支持。/ Only text and voice messages are supported.");
  if ((await settleHeldReplies(fromUser, text)) === "consumed") return;
  const approvalReply = parseApprovalReply(text);
  if (approvalReply) return decideByCode(fromUser, approvalReply);
  const command = parseWeixinCommand(text);
  const state = await threadStore.getChat(fromUser);
  if (command.name === "prompt" && state?.turnDelivery?.pendingQuestion && !state.turnDelivery.terminal && ownsRecovery(fromUser, state)) {
    return answerQuestion(fromUser, text);
  }
  await handleCommand(fromUser, command, {
    messageKey: inboundKey(msg),
    contextToken: msg.context_token,
  });
}

async function monitorLoop() {
  const { baseUrl, token } = botAccount;
  let getUpdatesBuf = await loadSyncBuf(config.stateDir);
  let nextTimeoutMs = config.longPollTimeoutMs;
  let consecutiveFailures = 0;

  console.log(`Monitor started: baseUrl=${baseUrl} timeoutMs=${nextTimeoutMs}`);

  while (!stopping) {
    if (sessionPaused()) {
      await sleep(Math.min(5000, sessionPausedUntil - Date.now()));
      continue;
    }
    try {
      const abortController = new AbortController();
      const timer = setTimeout(
        () => abortController.abort(),
        nextTimeoutMs + 5000
      );

      const resp = await getUpdates({
        baseUrl,
        token,
        get_updates_buf: getUpdatesBuf,
        timeoutMs: nextTimeoutMs,
        signal: abortController.signal,
      });

      clearTimeout(timer);

      if (resp.longpolling_timeout_ms) {
        nextTimeoutMs = resp.longpolling_timeout_ms;
      }

      // 检查错误
      const isApiError =
        (resp.ret !== undefined && resp.ret !== 0) ||
        (resp.errcode !== undefined && resp.errcode !== 0);

      if (isApiError && isSessionExpired(resp)) {
        pauseSession("getupdates");
        consecutiveFailures = 0;
        continue;
      }

      if (isApiError) {
        consecutiveFailures += 1;
        console.error(
          `getUpdates error: ret=${resp.ret} errcode=${resp.errcode} errmsg=${resp.errmsg}`
        );
        if (consecutiveFailures >= 3) {
          console.error("3 consecutive failures, backing off 30s");
          await sleep(30000);
          consecutiveFailures = 0;
        } else {
          await sleep(2000);
        }
        continue;
      }

      consecutiveFailures = 0;

      await processUpdateBatch({
        messages: resp.msgs,
        nextCursor: resp.get_updates_buf,
        store: threadStore,
        keyOf: (msg) => {
          const key = inboundKey(msg);
          if (!key) console.warn("Ignored inbound message without a stable account/peer/message identity.");
          return key;
        },
        handle: handleInbound,
        interrupted: async (msg) => {
          const key = inboundKey(msg);
          const state = await threadStore.getChat(msg.from_user_id);
          if (state?.pendingAdmission?.messageKey === key || state?.turnDelivery?.messageKey === key) {
            startChatRecovery(msg.from_user_id);
          } else if (commandAction(parseWeixinCommand(extractText(msg.item_list))).kind === "prompt" && !parseApprovalReply(extractText(msg.item_list))) {
            // Prompt admission always persists before side effects. No receipt
            // means this claim crashed before admission, so it can be handled.
            await handleInbound(msg);
          } else if (isAllowed(msg.from_user_id)) {
            await sendText(msg.from_user_id, "The bridge restarted while handling this command. Its effect is uncertain; check /status before repeating it.");
          }
        },
        commitCursor: async (cursor) => {
          await saveSyncBuf(config.stateDir, cursor);
          getUpdatesBuf = cursor;
        },
      });
      for (const [chatId, state] of threadStore.listChats()) {
        if (hasPendingWork(state)) startChatRecovery(chatId);
      }
    } catch (error) {
      if (error.name === "AbortError" || error.message?.includes("abort")) {
        // 长轮询超时是正常的，立即重试
        continue;
      }
      if (stopping) break;

      consecutiveFailures += 1;
      console.error(
        `getUpdates exception (${consecutiveFailures}/3):`,
        error.message
      );
      if (consecutiveFailures >= 3) {
        console.error("3 consecutive exceptions, backing off 30s");
        await sleep(30000);
        consecutiveFailures = 0;
      } else {
        await sleep(2000);
      }
    }
  }
}

function isAllowed(fromUser) {
  if (!fromUser) return false;
  if (config.allowUnlisted) return true;
  if (config.allowlist.length) return config.allowlist.includes(fromUser);
  return Boolean(botAccount?.userId) && fromUser === botAccount.userId;
}

function processAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}

async function acquireStateLock(stateDir) {
  const lockPath = path.join(stateDir, "bridge.lock");
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const handle = await fs.open(lockPath, "wx", 0o600);
      await handle.writeFile(String(process.pid));
      await handle.close();
      process.once("exit", () => {
        try {
          if (fsSync.readFileSync(lockPath, "utf8").trim() === String(process.pid)) fsSync.rmSync(lockPath, { force: true });
        } catch {}
      });
      return;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      const owner = Number((await fs.readFile(lockPath, "utf8").catch(() => "")).trim());
      if (owner && owner !== process.pid && processAlive(owner)) {
        throw new Error(`Another Weixin bridge (pid ${owner}) is using ${stateDir}. One state directory serves one bridge; stop it first.`);
      }
      await fs.rm(lockPath, { force: true });
    }
  }
  throw new Error(`Could not lock the state directory ${stateDir}.`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ============================================================================
// 启动流程 — QR 登录 → 长轮询
// ============================================================================

async function main() {
  console.log("Starting CodeWhale Weixin Bot Bridge");
  console.log(`Runtime: ${config.runtimeUrl}`);
  console.log(`Workspace: ${config.workspace}`);
  console.log(`State dir: ${config.stateDir}`);
  console.log(`Thread map: ${config.threadMapPath}`);

  // 初始化 ThreadStore。`open()` 只读，真正的写入发生在第一条消息到达时；
  // 那时失败会被 getUpdates 的 catch 吞掉，表现为“微信没有回应”。所以这里
  // 先建目录并真实写一次探针文件，把问题在启动时就暴露出来。
  try {
    const dir = path.dirname(config.threadMapPath);
    await fs.mkdir(dir, { recursive: true, mode: 0o700 });
    const probe = path.join(dir, ".write-probe");
    await fs.writeFile(probe, "", { mode: 0o600 });
    await fs.rm(probe, { force: true });
  } catch (error) {
    console.error(
      `Thread map directory is not writable: ${path.dirname(config.threadMapPath)} (${error.message})`
    );
    console.error(
      "Set WEIXIN_STATE_DIR (or WEIXIN_THREAD_MAP_PATH) to a writable directory."
    );
    process.exit(1);
  }
  try {
    await fs.mkdir(config.stateDir, { recursive: true, mode: 0o700 });
    await acquireStateLock(config.stateDir);
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
  threadStore = await CoreThreadStore.open(config.threadMapPath, { messageLimit: 500, privateMode: true });

  // 尝试加载已有账号
  botAccount = await loadAccount(config.stateDir);

  if (botAccount?.token) {
    console.log("Loaded existing bot account, trying to resume...");
    console.log(`  accountId: ${botAccount.accountId}`);
    console.log(`  baseUrl: ${botAccount.baseUrl}`);
  } else {
    // QR 登录
    console.log("No bot account found. Starting QR login...");
    console.log("");

    const { qrcodeUrl, sessionKey } = await getLoginQR();
    console.log("请用微信扫描以下二维码登录：");
    // Render the login URL as a scannable terminal QR. The URL is printed too,
    // so a terminal that mangles the half-block glyphs still has a way through.
    try {
      if (qrcodeUrl) console.log(renderQrToText(qrcodeUrl));
    } catch (error) {
      console.warn(`Could not render QR in terminal: ${error.message}`);
    }
    console.log(qrcodeUrl);
    console.log("");

    const result = await waitForLogin({ sessionKey, timeoutMs: 300_000 });

    if (!result.connected) {
      console.error(`Login failed: ${result.message}`);
      process.exit(1);
    }

    botAccount = {
      accountId: result.accountId,
      token: result.botToken,
      baseUrl: result.baseUrl,
      userId: result.userId,
    };

    await saveAccount(config.stateDir, botAccount);
    console.log(`✅ Login successful! accountId=${botAccount.accountId}`);
  }

  if (!accountIdentity()) throw new Error("The saved bot account has no stable account identity; pair again before receiving messages.");
  try { runtimeCapabilities = (await runtimeJson("/v1/runtime/info"))?.capabilities || {}; } catch { console.warn("Engine recovery capabilities unavailable; uncertain admissions will remain retained."); }
  for (const [chatId, state] of threadStore.listChats()) {
    if (hasPendingWork(state)) startChatRecovery(chatId);
  }

  // 通知上线
  try {
    const startResp = await notifyStart({
      baseUrl: botAccount.baseUrl,
      token: botAccount.token,
    });
    if (isSessionExpired(startResp)) {
      pauseSession("notifystart");
    } else if (startResp.ret && startResp.ret !== 0) {
      console.warn(`notifyStart: ret=${startResp.ret} errmsg=${startResp.errmsg}`);
    } else {
      console.log("notifyStart: OK");
    }
  } catch (error) {
    console.error("notifyStart failed:", error.message);
  }

  // 信号处理
  process.once("SIGINT", shutdown);
  process.once("SIGTERM", shutdown);

  if (!config.allowlist.length && !config.allowUnlisted) {
    console.log(botAccount.userId
      ? `No allowlist configured; only the WeChat user who scanned the login QR (user_id=${botAccount.userId}) may use this bridge. Other chats are refused once with their user ID.`
      : "No allowlist configured and the saved login has no scanner ID. Incoming chats are refused once with their user ID; set WEIXIN_CHAT_ALLOWLIST or scan a new QR code.");
  }

  // 进入长轮询循环
  await monitorLoop();

  console.log("Bridge stopped.");
}

async function shutdown() {
  if (stopping) return;
  stopping = true;
  console.log("Shutting down...");
  for (const controller of recoveryControllers.values()) controller.abort();
  for (const chatId of [...typingStates.keys()]) stopTyping(chatId);

  if (botAccount?.token) {
    try {
      const stopResp = await notifyStop({
        baseUrl: botAccount.baseUrl,
        token: botAccount.token,
      });
      console.log(
        `notifyStop: ret=${stopResp.ret} errmsg=${stopResp.errmsg ?? "OK"}`
      );
    } catch (error) {
      console.error("notifyStop failed:", error.message);
    }
  }

  setTimeout(() => process.exit(0), 2000);
}

main().catch((error) => {
  console.error("Fatal error:", error);
  process.exit(1);
});
