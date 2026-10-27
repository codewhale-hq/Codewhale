import { createHash } from "node:crypto";
import {
  activeTurnKeyboard,
  approvalKeyboard,
  callbackAction,
  commandAction,
  compactRuntimeError,
  controlKeyboard,
  envFirst,
  helpText,
  isAllowed,
  isGroupChat,
  isTelegramMarkdownParseError,
  latestRunningTurn,
  looksLikePollingConflict,
  pairingRefusalText,
  parseBool,
  parseCommand,
  parseList,
  preservedChatStateFields,
  splitMessage,
  stripGroupPrefix,
  threadListKeyboard,
  telegramIdentity,
  telegramMessageBody,
  telegramPollingConflictDelayMs,
  telegramRetryDelayMs,
  telegramSendRetryDelayMs
} from "./lib.mjs";
import {
  admitTurn,
  questionText,
  parseQuestionAnswers,
  createRuntimeClient,
  readJsonSafe,
  readSse,
  ThreadStore as CoreThreadStore,
  ApprovalOwnershipError,
  decideApproval as decideRuntimeApproval,
  settleStoredApproval
} from "../../bridge-core/src/lib.mjs";

const TYPING_INTERVAL_MS = 2000;
const TYPING_TIMEOUT_MS = 1500;
const LAST_SEQ_FLUSH_INTERVAL_MS = 2000;

class ThreadStore extends CoreThreadStore {
  constructor(filePath) {
    super(filePath, { messageLimit: 500, actions: true });
  }
}

const config = {
  botToken: requiredEnv("TELEGRAM_BOT_TOKEN"),
  apiBase: (process.env.TELEGRAM_API_BASE || "https://api.telegram.org").replace(/\/+$/, ""),
  runtimeUrl: (envFirst(process.env, "CODEWHALE_RUNTIME_URL", "DEEPSEEK_RUNTIME_URL") || "http://127.0.0.1:7878").replace(/\/+$/, ""),
  runtimeToken: requiredEnvFirst("CODEWHALE_RUNTIME_TOKEN", "DEEPSEEK_RUNTIME_TOKEN"),
  workspace: envFirst(process.env, "CODEWHALE_WORKSPACE", "DEEPSEEK_WORKSPACE") || process.cwd(),
  model: envFirst(process.env, "CODEWHALE_MODEL", "DEEPSEEK_MODEL") || "auto",
  mode: envFirst(process.env, "CODEWHALE_MODE", "DEEPSEEK_MODE") || "agent",
  allowShell: parseBool(envFirst(process.env, "CODEWHALE_ALLOW_SHELL", "DEEPSEEK_ALLOW_SHELL"), true),
  trustMode: parseBool(envFirst(process.env, "CODEWHALE_TRUST_MODE", "DEEPSEEK_TRUST_MODE"), false),
  autoApprove: parseBool(envFirst(process.env, "CODEWHALE_AUTO_APPROVE", "DEEPSEEK_AUTO_APPROVE"), false),
  allowlist: parseList(
    envFirst(process.env, "TELEGRAM_CHAT_ALLOWLIST", "CODEWHALE_CHAT_ALLOWLIST", "DEEPSEEK_CHAT_ALLOWLIST")
  ),
  allowUnlisted: parseBool(
    envFirst(process.env, "TELEGRAM_ALLOW_UNLISTED", "CODEWHALE_ALLOW_UNLISTED", "DEEPSEEK_ALLOW_UNLISTED"),
    false
  ),
  threadMapPath:
    process.env.TELEGRAM_THREAD_MAP_PATH ||
    "/var/lib/codewhale-telegram-bridge/thread-map.json",
  allowGroups: parseBool(process.env.TELEGRAM_ALLOW_GROUPS, false),
  requirePrefixInGroup: parseBool(process.env.TELEGRAM_REQUIRE_PREFIX_IN_GROUP, true),
  groupPrefix: process.env.TELEGRAM_GROUP_PREFIX || "/cw",
  maxReplyChars: Math.min(Number(process.env.TELEGRAM_MAX_REPLY_CHARS || 3500), 4096),
  pollTimeoutSeconds: Number(process.env.TELEGRAM_POLL_TIMEOUT_SECONDS || 50),
  turnTimeoutMs: Number(envFirst(process.env, "CODEWHALE_TURN_TIMEOUT_MS", "DEEPSEEK_TURN_TIMEOUT_MS") || 900000)
};

const { runtimeJson, authHeaders } = createRuntimeClient({ ...config, runtimeRequestTimeoutMs: 15000 });
const botAccountId = config.botToken.split(":")[0];
let runtimeCapabilities = {};

const threadStore = await ThreadStore.open(config.threadMapPath);
const activeTurnTasks = new Map();
let stopping = false;
let updateOffset = threadStore.getCursor(
  "telegram.update_offset",
  Number(process.env.TELEGRAM_UPDATE_OFFSET || 0)
);

function requestStop() {
  stopping = true;
  abortActiveTurnStreams();
}

process.once("SIGINT", requestStop);
process.once("SIGTERM", requestStop);

console.log("Starting CodeWhale Telegram bridge");
console.log(`Runtime: ${config.runtimeUrl}`);
console.log(`Workspace: ${config.workspace}`);
if (!config.allowlist.length && !config.allowUnlisted) {
  console.log("No allowlist configured. Incoming chats will receive their IDs and be refused.");
}

try {
  runtimeCapabilities = (await runtimeJson("/v1/runtime/info"))?.capabilities || {};
} catch {
  console.warn("Engine recovery capabilities unavailable; uncertain admissions will remain retained.");
}
await configureBotCommands().catch((error) => {
  console.error("failed to configure Telegram bot command menu", error);
});
void reattachActiveTurns().catch((error) => {
  console.error("failed to reattach active Telegram bridge turns", error);
});
await pollTelegram();

async function configureBotCommands() {
  await telegramApi("setMyCommands", {
    commands: [
      { command: "menu", description: "Open CodeWhale controls" },
      { command: "status", description: "Show runtime and workspace status" },
      { command: "threads", description: "List recent runtime threads" },
      { command: "new", description: "Create a new thread" },
      { command: "interrupt", description: "Interrupt the active turn" },
      { command: "answer", description: "Answer the current agent question" },
      { command: "compact", description: "Compact the current thread" },
      { command: "help", description: "Show command help" }
    ]
  });
}

async function pollTelegram() {
  let pollingConflictAttempts = 0;
  while (!stopping) {
    try {
      const updates = await telegramApi("getUpdates", {
        offset: updateOffset || undefined,
        timeout: config.pollTimeoutSeconds,
        allowed_updates: ["message", "callback_query"]
      });
      pollingConflictAttempts = 0;
      for (const update of updates || []) {
        try {
          await handleIncomingUpdate(update);
          await markUpdateHandled(update);
        } catch (error) {
          console.error("failed to handle incoming Telegram update", error);
          break;
        }
      }
    } catch (error) {
      if (looksLikePollingConflict(error)) {
        const waitMs = telegramPollingConflictDelayMs(pollingConflictAttempts);
        pollingConflictAttempts += 1;
        if (waitMs == null) {
          throw new Error(
            "Telegram getUpdates conflict; another bridge is polling this bot token. Stop the other bridge process or use a different token."
          );
        }
        console.warn(
          `Telegram getUpdates conflict; another bridge is polling this bot. Retrying in ${Math.round(waitMs / 1000)}s.`
        );
        await delay(waitMs);
        continue;
      }
      pollingConflictAttempts = 0;
      const waitMs = telegramRetryDelayMs(error);
      console.error(`Telegram poll failed: ${error.message}. Retrying in ${Math.round(waitMs / 1000)}s.`);
      await delay(waitMs);
    }
  }
}

async function markUpdateHandled(update) {
  if (update.update_id == null) return;
  if (!Number.isFinite(Number(update.update_id))) return;
  const nextOffset = Math.max(updateOffset, Number(update.update_id) + 1);
  if (nextOffset === updateOffset) return;
  updateOffset = nextOffset;
  await threadStore.setCursor("telegram.update_offset", updateOffset);
}

async function handleIncomingUpdate(update) {
  if (update.callback_query) {
    if (await isReplayCallbackUpdate(update)) return;
    await handleCallbackQuery(update.callback_query);
    return;
  }

  const identity = telegramIdentity(update);
  if (!identity.chatId || !identity.messageId) return;
  if (identity.isBot) return;

  if (!identity.text) {
    await sendText(identity.chatId, "Only text messages are supported in this first bridge.");
    return;
  }

  const scoped = stripGroupPrefix(identity.text, {
    chatType: identity.chatType,
    requirePrefix: config.requirePrefixInGroup,
    prefix: config.groupPrefix
  });
  if (!scoped.accepted) return;

  if (isGroupChat(identity.chatType) && !config.allowGroups) {
    await sendText(
      identity.chatId,
      "Group chat control is disabled for this bridge. DM the bot, or set TELEGRAM_ALLOW_GROUPS=true and allowlist this chat."
    );
    return;
  }

  if (!isAllowed(identity, config.allowlist, config.allowUnlisted)) {
    await sendText(identity.chatId, pairingRefusalText(identity));
    return;
  }

  const command = parseCommand(scoped.text);
  const messageKey = JSON.stringify([botAccountId, identity.chatId, identity.messageId]);
  if (["prompt", "answer"].includes(commandAction(command).kind)) {
    const state = await threadStore.getChat(identity.chatId);
    if (state?.pendingAdmission?.messageKey === messageKey || state?.activeMessageKey === messageKey) {
      await threadStore.save();
      startTrackedTurnStream(identity.chatId);
      return;
    }
  } else if (await threadStore.recordMessage(messageKey)) return;
  await rememberAuthorizedIdentity(identity);
  await handleCommand(identity.chatId, command, { ...identity, messageKey });
}

async function rememberAuthorizedIdentity({ chatId, chatType, userId, username, isBot }) {
  await threadStore.patchChat(chatId, {
    authorizedIdentity: { chatId, chatType, userId, username, isBot }
  });
}

async function isReplayCallbackUpdate(update) {
  if (update.update_id == null) return false;
  return threadStore.recordMessage(`callback:${update.update_id}`);
}

async function handleCommand(chatId, command, identity) {
  const action = commandAction(command);
  if (["new_thread", "resume", "compact"].includes(action.kind)) {
    const state = await threadStore.getChat(chatId);
    if (state?.pendingAdmission || state?.activeTurnId || state?.pendingReply) {
      await sendText(chatId, "This chat has a pending turn or retained reply. Use /status to inspect it before replacing its thread.");
      return;
    }
  }
  switch (action.kind) {
    case "help":
      await sendText(chatId, helpText(), { replyMarkup: controlKeyboard() });
      return;
    case "menu":
      await sendMenu(chatId);
      return;
    case "status":
      await sendStatus(chatId, identity);
      return;
    case "threads":
      await sendThreads(chatId, identity);
      return;
    case "new_thread": {
      const state = await ensureThread(chatId, { forceNew: true });
      await sendText(chatId, `Created thread ${state.threadId}`, { replyMarkup: controlKeyboard() });
      return;
    }
    case "resume":
      await resumeThread(chatId, action.threadId, identity);
      return;
    case "interrupt":
      await interruptActiveTurn(chatId, identity);
      return;
    case "compact":
      await compactThread(chatId);
      return;
    case "approval":
      await decideApproval(chatId, action, identity);
      return;
    case "set_model":
      await setChatModel(chatId, action.modelName);
      return;
    case "answer":
      await answerQuestion(chatId, action.text, identity);
      await threadStore.recordMessage(identity.messageKey);
      return;
    case "prompt":
      await startPromptTurn(chatId, action.prompt, identity);
      return;
    default:
      await sendText(chatId, helpText(), { replyMarkup: controlKeyboard() });
  }
}

async function handleCallbackQuery(query) {
  const chat = query.message?.chat || {};
  const from = query.from || {};
  const identity = {
    chatId: chat.id != null ? String(chat.id) : "",
    messageId: query.message?.message_id != null ? String(query.message.message_id) : "",
    chatType: chat.type || "",
    userId: from.id != null ? String(from.id) : "",
    username: from.username ? `@${from.username}` : "",
    firstName: from.first_name || "",
    isBot: Boolean(from.is_bot)
  };

  if (!identity.chatId || !query.id) return;
  if (identity.isBot) return;

  if (isGroupChat(identity.chatType) && !config.allowGroups) {
    await answerCallback(query.id, "Group control is disabled.");
    return;
  }
  if (!isAllowed(identity, config.allowlist, config.allowUnlisted)) {
    await answerCallback(query.id, "This chat is not allowlisted.");
    return;
  }

  const action = callbackAction(query.data);
  if (!action) {
    await answerCallback(query.id, "Unknown action.");
    return;
  }

  await rememberAuthorizedIdentity(identity);
  answerCallback(query.id, "Working...").catch((error) => {
    console.warn("failed to acknowledge Telegram callback", error);
  });
  await handleModalAction(identity.chatId, action, query, identity);
}

async function handleModalAction(chatId, action, query = null, identity = null) {
  if (["new_thread", "compact"].includes(action.kind)) {
    const state = await threadStore.getChat(chatId);
    if (state?.pendingAdmission || state?.activeTurnId || state?.pendingReply) {
      await sendText(chatId, "This chat has a pending turn or retained reply. Use /status to inspect it before replacing its thread.");
      return;
    }
  }
  switch (action.kind) {
    case "help":
      await sendText(chatId, helpText(), { replyMarkup: controlKeyboard() });
      return;
    case "status":
      await sendStatus(chatId, identity);
      return;
    case "threads":
      await sendThreads(chatId, identity);
      return;
    case "new_thread": {
      const state = await ensureThread(chatId, { forceNew: true });
      await sendText(chatId, `Created thread ${state.threadId}`, { replyMarkup: controlKeyboard() });
      return;
    }
    case "interrupt":
      await interruptActiveTurn(chatId, identity);
      return;
    case "compact":
      await compactThread(chatId);
      return;
    case "set_model":
      await setChatModel(chatId, action.modelName);
      return;
    case "stored_action":
      await handleStoredAction(chatId, action, query, identity);
      return;
    default:
      await sendText(chatId, helpText(), { replyMarkup: controlKeyboard() });
  }
}

async function handleStoredAction(chatId, action, query = null, identity = null) {
  // Approval buttons also belong to the thread still bound to this chat.
  const state = await threadStore.getChat(chatId);
  const owner = { chatId, threadId: state?.threadId, actorId: turnActor(identity) };
  const stored = await threadStore.getAction(action.token, owner);
  if (!stored) {
    await sendText(chatId, "That action expired. Open /menu and try again.");
    return;
  }

  if (stored.kind === "resume") {
    await threadStore.takeAction(action.token, owner);
    await resumeThread(chatId, stored.threadId, identity);
    return;
  }

  if (stored.kind === "approval") {
    const suffix = action.suffix || "";
    const decision = suffix === "deny" ? "deny" : "allow";
    const remember = suffix === "remember";
    // The token is consumed only after the Runtime accepted the decision.
    try {
      await settleStoredApproval(threadStore, action.token, owner, async () => {
        if (!(await decideApproval(chatId, { decision, approvalId: stored.approvalId, remember, turnId: stored.owner.turnId }, identity))) {
          throw new ApprovalOwnershipError("not delivered");
        }
      });
    } catch (error) {
      if (error instanceof ApprovalOwnershipError) return;
      throw error;
    }
    if (query?.message?.message_id) {
      await editMessageReplyMarkup(chatId, query.message.message_id, null).catch(() => {});
    }
    return;
  }

  await sendText(chatId, "That action is no longer supported.");
}

async function sendMenu(chatId) {
  const state = await threadStore.getChat(chatId);
  await sendText(
    chatId,
    [
      "CodeWhale controls",
      state?.threadId ? `thread=${state.threadId}` : "thread=(new on first prompt)",
      `model=${state?.model || config.model}`
    ].join("\n"),
    { replyMarkup: controlKeyboard() }
  );
}

async function ensureThread(chatId, { forceNew = false, threadRequest } = {}) {
  const existing = await threadStore.getChat(chatId);
  if (forceNew && (existing?.pendingAdmission || existing?.activeTurnId)) {
    throw new Error("This chat has a pending turn or retained reply. Use /status to inspect it before replacing its thread.");
  }
  if (existing?.threadId && !forceNew) return existing;

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
      system_prompt:
        "You are being controlled from a Telegram phone chat. Keep status updates concise. Ask for tool approvals when needed; do not assume mobile messages imply blanket approval."
    }
  });

  const state = {
    ...preservedChatStateFields(existing, ["model", "authorizedIdentity", "pendingAdmission", "pendingReply"]),
    threadId: thread.id,
    lastSeq: 0,
    activeTurnId: null,
    updatedAt: new Date().toISOString()
  };
  await threadStore.setChat(chatId, state);
  return state;
}

async function startPromptTurn(chatId, prompt, identity) {
  const current = await threadStore.getChat(chatId);
  if (current?.pendingQuestion) {
    await answerQuestion(chatId, prompt, identity);
    await threadStore.recordMessage(identity.messageKey);
    return;
  }
  if (current?.pendingReply) {
    await sendText(chatId, "The previous reply is retained because delivery was not confirmed. Its initiating human can retrieve it with /status before starting another turn.");
    return;
  }
  if (activeTurnTasks.has(chatId) || current?.pendingAdmission || current?.activeTurnId) {
    startTrackedTurnStream(chatId);
    await sendText(chatId, "This chat has a pending turn. Use /status or /interrupt.", {
      replyMarkup: activeTurnKeyboard()
    });
    return;
  }
  if (!prompt.trim()) return sendText(chatId, helpText(), { replyMarkup: controlKeyboard() });
  const actorId = turnActor(identity);
  if (!actorId || !identity.messageKey) throw new ApprovalOwnershipError("turn needs its initiating human and message");
  const request = {
    prompt, input_summary: prompt.slice(0, 200), model: current?.model || config.model,
    mode: config.mode, allow_shell: config.allowShell, trust_mode: config.trustMode,
    auto_approve: config.autoApprove
  };
  const operationKey = supportsAdmissionRecovery()
    ? createHash("sha256").update(identity.messageKey).digest("hex") : null;
  if (operationKey) request.operation_key = operationKey;
  const replay = await threadStore.recordMessage(identity.messageKey, chatId, { pendingAdmission: {
    botAccountId, runtimeUrl: config.runtimeUrl, actorId, identity: {
      chatId, chatType: identity.chatType, userId: identity.userId,
      username: identity.username, isBot: identity.isBot
    }, messageKey: identity.messageKey, request, operationKey,
    threadId: current?.threadId || null, submitted: false, sinceSeq: null,
    threadRequest: {
      model: request.model, workspace: config.workspace, mode: request.mode,
      allow_shell: request.allow_shell, trust_mode: request.trust_mode,
      auto_approve: request.auto_approve, archived: false,
      system_prompt: "You are being controlled from a Telegram phone chat. Keep status updates concise. Ask for tool approvals when needed; do not assume mobile messages imply blanket approval."
    }
  } });
  if (!replay) startTrackedTurnStream(chatId);
}

function abortActiveTurnStreams() {
  for (const task of activeTurnTasks.values()) task.controller?.abort();
}

async function clearActiveTurn(chatId) {
  await threadStore.patchChat(chatId, {
    activeTurnId: null, activeMessageKey: null, activeIdentity: null, activeBotAccountId: null, activeRuntimeUrl: null,
    responseText: "", turnTerminal: false, pendingQuestion: null, updatedAt: new Date().toISOString()
  });
}

function recoveryAllowed(chatId, state) {
  const pending = state?.pendingAdmission;
  const identity = pending?.identity || state?.activeIdentity || state?.authorizedIdentity;
  const accountId = pending?.botAccountId || state?.activeBotAccountId;
  const runtimeUrl = pending?.runtimeUrl || state?.activeRuntimeUrl;
  return (!runtimeUrl || runtimeUrl === config.runtimeUrl) && (!accountId || accountId === botAccountId) && identity && identity.chatId === chatId && !identity.isBot &&
    ["private", "group", "supergroup"].includes(identity.chatType) &&
    (!isGroupChat(identity.chatType) || config.allowGroups) &&
    isAllowed(identity, config.allowlist, config.allowUnlisted);
}

function supportsAdmissionRecovery() {
  return runtimeCapabilities.turn_operation_idempotency === true && runtimeCapabilities.turn_operation_lookup === true;
}

function startTrackedTurnStream(chatId) {
  if (stopping || activeTurnTasks.has(chatId)) return false;
  const controller = new AbortController();
  const task = { controller };
  activeTurnTasks.set(chatId, task);
  void (async () => {
    while (!stopping && !controller.signal.aborted) {
      const state = await threadStore.getChat(chatId);
      if (!recoveryAllowed(chatId, state)) return;
      if (!state?.pendingAdmission && !state?.activeTurnId) return;
      try {
        if (await runPrompt(chatId, { signal: controller.signal })) return;
        const pending = (await threadStore.getChat(chatId))?.pendingAdmission;
        if (pending?.submitted && (!pending.operationKey || !supportsAdmissionRecovery())) return;
      } catch (error) {
        console.error("Telegram turn recovery deferred; original request retained", error);
      }
      if (!stopping && !controller.signal.aborted) await delay(2000);
    }
  })().catch((error) => {
    console.error("Telegram recovery stopped; persisted state retained", error);
  }).finally(() => {
    if (activeTurnTasks.get(chatId) === task) activeTurnTasks.delete(chatId);
  });
  return true;
}

function turnActor(identity) {
  return identity?.userId && !identity.isBot ? `telegram-user:${identity.userId}` : "";
}

async function runPrompt(chatId, options = {}) {
  let state = await threadStore.getChat(chatId);
  if (!recoveryAllowed(chatId, state)) return true;
  let pending = state?.pendingAdmission;
  if (pending) {
    const admission = await admitTurn({
      runtimeJson, pending, capabilities: runtimeCapabilities,
      persistPending: (value) => threadStore.patchChat(chatId, { pendingAdmission: value }),
      ensureThread: async (threadRequest) => (await ensureThread(chatId, { threadRequest })).threadId
    });
    pending = admission.pending;
    const turn = admission.turn;
    if (!turn) {
      if (admission.blocked && !pending.submitted) {
        await threadStore.patchChat(chatId, { pendingAdmission: null });
        await sendText(chatId, "Thread already has an active turn. Wait for it to finish or send /interrupt.", { replyMarkup: activeTurnKeyboard() });
        return true;
      }
      return admission.uncertain === true;
    }
    await threadStore.recordTurnOrigin(chatId, pending.threadId, turn.id, pending.actorId);
    state = await threadStore.patchChat(chatId, {
      pendingAdmission: null, activeTurnId: turn.id, activeMessageKey: pending.messageKey,
      activeIdentity: pending.identity, activeBotAccountId: pending.botAccountId,
      activeRuntimeUrl: pending.runtimeUrl,
      lastSeq: pending.sinceSeq, responseText: "", turnTerminal: false,
      updatedAt: new Date().toISOString()
    });
    await sendTurnText(chatId, `Started turn ${turn.id}`, { replyMarkup: activeTurnKeyboard() });
  }
  if (!state?.activeTurnId) return true;
  if (state.turnTerminal || await streamTurnEvents(chatId, state.threadId, state.activeTurnId, Number(state.lastSeq || 0), options)) {
    await clearActiveTurn(chatId);
    return true;
  }
  return false;
}

async function reattachActiveTurns() {
  for (const [chatId, state] of threadStore.listChats()) {
    if ((!state?.pendingAdmission && !state?.activeTurnId) || !recoveryAllowed(chatId, state)) continue;
    startTrackedTurnStream(chatId);
  }
}

async function streamTurnEvents(chatId, threadId, turnId, sinceSeq, options = {}) {
  const controller = new AbortController();
  let timedOut = false;
  const timeout = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, config.turnTimeoutMs);
  const abortFromCaller = () => controller.abort();
  if (options.signal?.aborted) {
    controller.abort();
  } else {
    options.signal?.addEventListener("abort", abortFromCaller, { once: true });
  }
  let responseText = (await threadStore.getChat(chatId))?.responseText || "";
  let turnTerminal = false;
  let latestSeq = sinceSeq;
  let flushedSeq = sinceSeq;
  let lastSeqFlushAt = 0;
  let sentProgressAt = Date.now();
  let typingPaused = false;
  let typingInFlight = false;

  async function flushLastSeq(force = false) {
    if (latestSeq <= flushedSeq) return;
    if (!force && Date.now() - lastSeqFlushAt < LAST_SEQ_FLUSH_INTERVAL_MS) return;
    await threadStore.patchChat(chatId, { lastSeq: latestSeq, responseText, turnTerminal });
    flushedSeq = latestSeq;
    lastSeqFlushAt = Date.now();
  }

  const tickTyping = async () => {
    if (stopping || typingPaused || typingInFlight) return;
    typingInFlight = true;
    try {
      await sendTypingAction(chatId);
    } catch (error) {
      console.warn("failed to send Telegram typing action", error);
    } finally {
      typingInFlight = false;
    }
  };
  const typingTimer = setInterval(() => {
    void tickTyping();
  }, TYPING_INTERVAL_MS);
  typingTimer.unref?.();

  try {
    void tickTyping();
    const response = await fetch(
      `${config.runtimeUrl}/v1/threads/${encodeURIComponent(threadId)}/events?since_seq=${sinceSeq}`,
      {
        headers: authHeaders(),
        signal: controller.signal
      }
    );
    if (!response.ok) {
      const body = await readJsonSafe(response);
      throw new Error(compactRuntimeError(response.status, body));
    }

    for await (const event of readSse(response)) {
      if (!event.data) continue;
      const record = JSON.parse(event.data);
      latestSeq = Math.max(latestSeq, Number(record.seq || 0));

      if (turnId && record.turn_id && record.turn_id !== turnId) continue;
      const lifecycleStatus =
        record.event === "turn.lifecycle"
          ? record.payload?.turn?.status || record.payload?.status
          : null;
      const stopTypingEvent =
        record.event === "turn.completed" ||
        ["failed", "canceled", "interrupted"].includes(lifecycleStatus);
      if (typingPaused && !["approval.required", "user_input.required"].includes(record.event) && !stopTypingEvent) {
        typingPaused = false;
        void tickTyping();
      }

      if (record.event === "item.delta" && record.payload?.kind === "agent_message") {
        responseText += record.payload.delta || "";
        const now = Date.now();
        if (responseText.length > config.maxReplyChars && now - sentProgressAt > 15000) {
          await sendTurnText(chatId, responseText.slice(0, config.maxReplyChars));
          responseText = responseText.slice(config.maxReplyChars);
          sentProgressAt = now;
        }
      }

      if (record.event === "user_input.required") {
        typingPaused = true;
        const input = record.payload || {};
        const inputId = input.input_id || input.id;
        if (inputId && input.request) {
          await threadStore.patchChat(chatId, { pendingQuestion: { inputId, request: input.request, turnId } });
          await sendTurnText(chatId, `${questionText(input.request)}\nUse /answer followed by your response.`, { replyMarkup: activeTurnKeyboard() });
        }
      }
      if (["user_input.answered", "user_input.canceled"].includes(record.event)) {
        const state = await threadStore.getChat(chatId);
        if (state?.pendingQuestion?.inputId === (record.payload?.input_id || record.payload?.id)) {
          await threadStore.patchChat(chatId, { pendingQuestion: null });
        }
      }

      if (record.event === "approval.required") {
        typingPaused = true;
        const approval = record.payload || {};
        const approvalId = approval.approval_id || approval.id;
        if (!approvalId) {
          await sendTurnText(
            chatId,
            [
              "Approval required",
              `tool=${approval.tool_name || "unknown"}`,
              approval.description || "",
              "",
              "No approval_id was provided by the runtime; use /status and retry from the TUI."
            ]
              .filter(Boolean)
              .join("\n"),
            { replyMarkup: controlKeyboard() }
          );
          continue;
        }
        const origin = threadStore.turnOrigin(chatId, threadId, turnId);
        if (!origin) {
          await sendTurnText(chatId, "Approval has no recorded initiating human; decide from the TUI.");
          continue;
        }
        const actionToken = await threadStore.putAction(
          { kind: "approval", approvalId },
          { chatId, threadId, turnId, actorId: origin.actorId }
        );
        await sendTurnText(
          chatId,
          [
            "Approval required",
            `tool=${approval.tool_name || "unknown"}`,
            `approval_id=${approvalId}`,
            approval.description || "",
            "",
            `Tap a button, or reply /allow ${approvalId}`,
            `Reply /deny ${approvalId}`
          ]
            .filter(Boolean)
            .join("\n"),
          { replyMarkup: approvalKeyboard(actionToken) }
        );
      }

      if (record.event === "turn.completed") {
        typingPaused = true;
        const turn = record.payload?.turn || {};
        const status = turn.status || "completed";
        const error = turn.error ? `\n${turn.error}` : "";
        if (status !== "completed") {
          await sendTurnText(chatId, `Turn ${status}.${error}`.trim(), {
            replyMarkup: controlKeyboard(), retain: true
          });
        } else {
          await sendTurnText(chatId, responseText.trim() || "Turn completed.", {
            replyMarkup: controlKeyboard(), retain: true
          });
        }
        turnTerminal = true;
        return true;
      }

      if (record.event === "turn.lifecycle") {
        if (["failed", "canceled", "interrupted"].includes(lifecycleStatus)) {
          typingPaused = true;
          await sendTurnText(chatId, `Turn ${lifecycleStatus}.`, { replyMarkup: controlKeyboard(), retain: true });
          turnTerminal = true;
          return true;
        }
      }
      await flushLastSeq(false);
    }
  } catch (error) {
    if (error.name === "AbortError") {
      if (timedOut) {
        await sendTurnText(chatId, "Observation timed out; the bridge will reconnect to the same turn.");
      } else if (!stopping) {
        await sendTurnText(chatId, "Turn stream aborted.");
      }
      return;
    }
    throw error;
  } finally {
    clearInterval(typingTimer);
    clearTimeout(timeout);
    options.signal?.removeEventListener("abort", abortFromCaller);
    await flushLastSeq(true);
  }
}

async function answerQuestion(chatId, text, identity) {
  const state = await threadStore.getChat(chatId);
  if (!state?.threadId || !state.activeTurnId || !recoveryAllowed(chatId, state) ||
      threadStore.turnOrigin(chatId, state.threadId, state.activeTurnId)?.actorId !== turnActor(identity)) {
    await sendText(chatId, "Only the recorded initiating human can answer this turn's question.");
    return;
  }
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}`);
  const inputs = (detail.pending_user_inputs || []).filter((input) => input.turn_id === state.activeTurnId);
  const input = state.pendingQuestion
    ? inputs.find((candidate) => (candidate.id || candidate.input_id) === state.pendingQuestion.inputId)
    : inputs.length === 1 ? inputs[0] : null;
  const inputId = input?.id || input?.input_id;
  if (!inputId) {
    await threadStore.patchChat(chatId, { pendingQuestion: null });
    await sendText(chatId, "No matching question is waiting in this turn.");
    return;
  }
  const answers = parseQuestionAnswers(input.request, text);
  if (!answers) {
    await sendText(chatId, questionText(input.request));
    return;
  }
  await runtimeJson(`/v1/user-input/${encodeURIComponent(state.threadId)}/${encodeURIComponent(inputId)}`, {
    method: "POST", body: { answers }
  });
  if ((await threadStore.getChat(chatId))?.pendingQuestion?.inputId === inputId) {
    await threadStore.patchChat(chatId, { pendingQuestion: null });
  }
  await sendText(chatId, "Answer sent.");
}

async function sendStatus(chatId, identity) {
  const state = await threadStore.getChat(chatId);
  const reply = state?.pendingReply;
  if (reply && recoveryAllowed(chatId, { pendingAdmission: reply }) && reply.actorId === turnActor(identity)) {
    await sendText(chatId, `Retained reply (the earlier delivery may have succeeded):\n\n${reply.text}`, { replyMarkup: controlKeyboard() });
    if ((await threadStore.getChat(chatId))?.pendingReply?.key === reply.key) {
      await threadStore.patchChat(chatId, { pendingReply: null });
    }
  }
  const [health, runtimeInfo, workspace] = await Promise.all([
    runtimeJson("/health", { auth: false }),
    runtimeJson("/v1/runtime/info"),
    runtimeJson("/v1/workspace/status")
  ]);
  await sendText(
    chatId,
    [
      `runtime=${health.status || "unknown"}`,
      `version=${runtimeInfo.version || "unknown"}`,
      `bind=${runtimeInfo.bind_host}:${runtimeInfo.port}`,
      `auth_required=${runtimeInfo.auth_required}`,
      `workspace=${workspace.workspace}`,
      `git_repo=${workspace.git_repo}`,
      workspace.branch ? `branch=${workspace.branch}` : "",
      `staged=${workspace.staged} unstaged=${workspace.unstaged} untracked=${workspace.untracked}`,
      state?.pendingAdmission ? "The original prompt is retained while Engine admission is reconciled." : "",
      state?.pendingAdmission?.submitted && !state.pendingAdmission.operationKey ? "This Engine cannot reconcile the uncertain admission; inspect the Runtime before repeating the prompt." : "",
      state?.activeTurnId ? `turn=${state.activeTurnId}` : "",
      state?.pendingQuestion ? questionText(state.pendingQuestion.request) : ""
    ]
      .filter(Boolean)
      .join("\n"),
    { replyMarkup: controlKeyboard() }
  );
}

async function sendThreads(chatId, identity) {
  const state = await threadStore.getChat(chatId);
  const threads = isGroupChat(identity?.chatType)
    ? state?.threadId ? [{ id: state.threadId }] : []
    : await runtimeJson("/v1/threads/summary?limit=8&include_archived=true");
  if (!threads.length) {
    await sendText(chatId, "No runtime threads yet.", { replyMarkup: controlKeyboard() });
    return;
  }
  const actions = [];
  for (const [index, thread] of threads.slice(0, 8).entries()) {
    const token = await threadStore.putAction(
      { kind: "resume", threadId: thread.id },
      { chatId }
    );
    actions.push({ token, label: `Resume ${index + 1}` });
  }
  await sendText(
    chatId,
    threads
      .map((thread, index) => {
        const status = thread.latest_turn_status || "none";
        return `${index + 1}. ${thread.id} [${status}] ${thread.title || thread.preview || ""}`;
      })
      .join("\n"),
    { replyMarkup: threadListKeyboard(actions) }
  );
}

async function resumeThread(chatId, args, identity) {
  const existing = await threadStore.getChat(chatId);
  if (existing?.pendingAdmission || existing?.activeTurnId || existing?.pendingReply) {
    await sendText(chatId, "This chat has a pending turn or retained reply. Use /status to inspect it before replacing its thread.");
    return;
  }
  const threadId = args.trim();
  if (!threadId) {
    await sendText(chatId, "Usage: /resume <thread_id>");
    return;
  }
  if (isGroupChat(identity?.chatType) && (threadId !== existing?.threadId ||
      !(existing?.turnOrigins || []).some((origin) => origin.threadId === threadId && origin.actorId === turnActor(identity)))) {
    await sendText(chatId, "Group chats can only resume their own thread through a recorded initiating human.");
    return;
  }
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(threadId)}`);
  await threadStore.setChat(chatId, {
    ...preservedChatStateFields(existing),
    threadId,
    lastSeq: Number(detail.latest_seq || 0),
    activeTurnId: null,
    updatedAt: new Date().toISOString()
  });
  await sendText(chatId, `Resumed thread ${threadId}`, { replyMarkup: controlKeyboard() });
}

async function interruptActiveTurn(chatId, identity) {
  const state = await threadStore.getChat(chatId);
  if (state?.pendingAdmission) {
    if (state.pendingAdmission.actorId !== turnActor(identity)) {
      await sendText(chatId, "Only the initiating human can control this pending turn.");
      return;
    }
    startTrackedTurnStream(chatId);
    await sendText(chatId, "The original prompt's admission is still being reconciled. Use /status, then /interrupt once its turn is known.");
    return;
  }
  if (!state?.threadId) {
    await sendText(chatId, "No runtime thread recorded for this chat.");
    return;
  }
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}`);
  const runningTurn = latestRunningTurn(detail);
  const turnId = state.activeTurnId || runningTurn?.id;
  if (!turnId) {
    await sendText(chatId, "No active turn recorded for this chat.");
    return;
  }
  if (threadStore.turnOrigin(chatId, state.threadId, turnId)?.actorId !== turnActor(identity)) {
    await sendText(chatId, "Only the recorded initiating human can interrupt this turn here. Use the Runtime for an unbound turn.");
    return;
  }
  await runtimeJson(
    `/v1/threads/${encodeURIComponent(state.threadId)}/turns/${encodeURIComponent(
      turnId
    )}/interrupt`,
    { method: "POST" }
  );
  await threadStore.patchChat(chatId, {
    activeTurnId: turnId,
    updatedAt: new Date().toISOString()
  });
  await sendText(chatId, `Interrupt requested for ${turnId}`, { replyMarkup: controlKeyboard() });
}

async function compactThread(chatId) {
  const state = await ensureThread(chatId);
  const result = await runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}/compact`, {
    method: "POST",
    body: { reason: "telegram bridge request" }
  });
  await sendText(chatId, `Compaction started: ${result.turn?.id || "unknown turn"}`, {
    replyMarkup: activeTurnKeyboard()
  });
}

/** Deliver a decision for an approval pending on this chat's thread. */
async function decideApproval(chatId, action, identity) {
  const decision = action.decision;
  const { approvalId, remember } = action;
  if (!approvalId) {
    await sendText(
      chatId,
      `Usage: /${decision} <approval_id>${decision === "allow" ? " [remember]" : ""}`
    );
    return false;
  }
  try {
    await decideRuntimeApproval(runtimeJson, { store: threadStore, chatId, actorId: turnActor(identity), approvalId, decision, remember, turnId: action.turnId });
  } catch (error) {
    if (!(error instanceof ApprovalOwnershipError)) throw error;
    await sendText(chatId, `Approval ${approvalId} is not waiting in this chat.`);
    return false;
  }
  await sendText(chatId, `Approval ${approvalId}: ${decision}${remember ? " and remember" : ""}`);
  return true;
}

async function setChatModel(chatId, modelName) {
  if (!modelName || modelName === "default") {
    await threadStore.patchChat(chatId, {
      model: null,
      updatedAt: new Date().toISOString()
    });
    await sendText(chatId, `Reset per-chat model. Using bridge default: ${config.model}`, {
      replyMarkup: controlKeyboard()
    });
    return;
  }
  await threadStore.patchChat(chatId, {
    model: modelName,
    updatedAt: new Date().toISOString()
  });
  await sendText(chatId, `Per-chat model set to: ${modelName}`, { replyMarkup: controlKeyboard() });
}

async function sendText(chatId, text, options = {}) {
  const chunks = splitMessage(text, config.maxReplyChars);
  for (const [index, chunk] of chunks.entries()) {
    const body = {
      chat_id: chatId,
      ...telegramMessageBody(chunk, { markdown: true, maxChars: config.maxReplyChars }),
      disable_web_page_preview: true
    };
    if (options.replyMarkup && index === chunks.length - 1) {
      body.reply_markup = options.replyMarkup;
    }
    try {
      await telegramApi("sendMessage", body);
    } catch (error) {
      if (!isTelegramMarkdownParseError(error)) throw error;
      const fallbackBody = {
        chat_id: chatId,
        ...telegramMessageBody(chunk, { markdown: false, maxChars: config.maxReplyChars }),
        disable_web_page_preview: true
      };
      if (options.replyMarkup && index === chunks.length - 1) {
        fallbackBody.reply_markup = options.replyMarkup;
      }
      await telegramApi("sendMessage", fallbackBody);
    }
  }
}

async function sendTypingAction(chatId) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), TYPING_TIMEOUT_MS);
  try {
    await telegramApi(
      "sendChatAction",
      {
        chat_id: chatId,
        action: "typing"
      },
      { signal: controller.signal }
    );
  } finally {
    clearTimeout(timeout);
  }
}

async function sendTurnText(chatId, text, options = {}) {
  let reply;
  if (options.retain) {
    const state = await threadStore.getChat(chatId);
    reply = {
      key: createHash("sha256").update(JSON.stringify([state.threadId, state.activeTurnId, text])).digest("hex"),
      text, identity: state.activeIdentity, actorId: turnActor(state.activeIdentity),
      botAccountId: state.activeBotAccountId, runtimeUrl: state.activeRuntimeUrl
    };
    await threadStore.patchChat(chatId, { pendingReply: reply, turnTerminal: true });
  }
  try {
    await sendText(chatId, text, options);
    if (reply && (await threadStore.getChat(chatId))?.pendingReply?.key === reply.key) {
      await threadStore.patchChat(chatId, { pendingReply: null });
    }
  } catch (error) {
    console.error("failed to send Telegram turn update", error);
  }
}

async function answerCallback(callbackQueryId, text = "") {
  await telegramApi("answerCallbackQuery", {
    callback_query_id: callbackQueryId,
    text: text.slice(0, 200),
    show_alert: false
  });
}

async function editMessageReplyMarkup(chatId, messageId, replyMarkup) {
  await telegramApi("editMessageReplyMarkup", {
    chat_id: chatId,
    message_id: messageId,
    reply_markup: replyMarkup
  });
}

async function telegramApi(method, body = {}, options = {}) {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await telegramApiOnce(method, body, options);
    } catch (error) {
      const retryMs = method === "sendMessage" ? telegramSendRetryDelayMs(error, attempt) : null;
      if (retryMs == null) throw error;
      console.warn(
        `Telegram ${method} failed: ${error.message}. Retrying in ${Math.round(retryMs / 1000)}s.`
      );
      await delay(retryMs);
    }
  }
}

async function telegramApiOnce(method, body = {}, options = {}) {
  const response = await fetch(`${config.apiBase}/bot${config.botToken}/${method}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal: options.signal || AbortSignal.timeout(method === "getUpdates" ? (config.pollTimeoutSeconds + 15) * 1000 : 15000)
  });
  const payload = await readJsonSafe(response);
  if (!response.ok || payload?.ok === false) {
    const error = new Error(
      payload?.description || `Telegram API request failed (${response.status})`
    );
    error.errorCode = payload?.error_code || response.status;
    error.description = payload?.description || "";
    error.parameters = payload?.parameters || {};
    throw error;
  }
  return payload.result;
}

function requiredEnv(name) {
  const value = process.env[name];
  if (!value || !value.trim()) {
    throw new Error(`${name} is required`);
  }
  return value.trim();
}

function requiredEnvFirst(...names) {
  const value = envFirst(process.env, ...names);
  if (!value) {
    throw new Error(`${names.join(" or ")} is required`);
  }
  return value;
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
