import * as Lark from "@larksuiteoapi/node-sdk";

import {
  activeTurnBlock,
  commandAction,
  compactRuntimeError,
  helpText,
  incomingIdentity,
  isAllowed,
  latestRunningTurn,
  pairingRefusalText,
  parseBool,
  parseCommand,
  parseList,
  parseApprovalDecisionArgs,
  parseTextContent,
  preservedChatStateFields,
  splitMessage,
  stripGroupPrefix
} from "./lib.mjs";
import {
  ApprovalOwnershipError,
  createRuntimeClient,
  decideApproval as decideRuntimeApproval,
  readJsonSafe,
  readThreadEvents,
  ThreadStore as CoreThreadStore
} from "../../bridge-core/src/lib.mjs";

class ThreadStore extends CoreThreadStore {
  constructor(filePath) {
    super(filePath, { messageLimit: 200 });
  }
}

const config = {
  appId: requiredEnv("FEISHU_APP_ID"),
  appSecret: requiredEnv("FEISHU_APP_SECRET"),
  domain: process.env.FEISHU_DOMAIN || "feishu",
  runtimeUrl: (process.env.CODEWHALE_RUNTIME_URL || process.env.DEEPSEEK_RUNTIME_URL || "http://127.0.0.1:7878").replace(/\/+$/, ""),
  runtimeToken: process.env.CODEWHALE_RUNTIME_TOKEN || process.env.DEEPSEEK_RUNTIME_TOKEN || requiredEnv("CODEWHALE_RUNTIME_TOKEN"),
  workspace: process.env.CODEWHALE_WORKSPACE || process.env.DEEPSEEK_WORKSPACE || process.cwd(),
  model: process.env.CODEWHALE_MODEL || process.env.DEEPSEEK_MODEL || "auto",
  mode: process.env.CODEWHALE_MODE || process.env.DEEPSEEK_MODE || "agent",
  allowShell: parseBool(process.env.CODEWHALE_ALLOW_SHELL ?? process.env.DEEPSEEK_ALLOW_SHELL, true),
  trustMode: parseBool(process.env.CODEWHALE_TRUST_MODE ?? process.env.DEEPSEEK_TRUST_MODE, false),
  autoApprove: parseBool(process.env.CODEWHALE_AUTO_APPROVE ?? process.env.DEEPSEEK_AUTO_APPROVE, false),
  allowlist: parseList(process.env.CODEWHALE_CHAT_ALLOWLIST || process.env.DEEPSEEK_CHAT_ALLOWLIST),
  allowUnlisted: parseBool(process.env.CODEWHALE_ALLOW_UNLISTED ?? process.env.DEEPSEEK_ALLOW_UNLISTED, false),
  threadMapPath:
    process.env.FEISHU_THREAD_MAP_PATH ||
    "/var/lib/codewhale-feishu-bridge/thread-map.json",
  allowGroups: parseBool(process.env.FEISHU_ALLOW_GROUPS, false),
  requirePrefixInGroup: parseBool(process.env.FEISHU_REQUIRE_PREFIX_IN_GROUP, true),
  groupPrefix: process.env.FEISHU_GROUP_PREFIX || "/ds",
  maxReplyChars: Number(process.env.FEISHU_MAX_REPLY_CHARS || 3500),
  turnTimeoutMs: Number(process.env.CODEWHALE_TURN_TIMEOUT_MS || process.env.DEEPSEEK_TURN_TIMEOUT_MS || 900000)
};

const { runtimeJson, authHeaders } = createRuntimeClient(config);

const sdkConfig = {
  appId: config.appId,
  appSecret: config.appSecret,
  domain: resolveLarkDomain(config.domain)
};

const client = new Lark.Client(sdkConfig);
const wsClient = new Lark.WSClient({
  ...sdkConfig,
  loggerLevel: Lark.LoggerLevel?.info
});

const threadStore = await ThreadStore.open(config.threadMapPath);

const dispatcher = new Lark.EventDispatcher({}).register({
  "im.message.receive_v1": async (data) => {
    void handleIncomingMessage(data).catch((error) => {
      console.error("failed to handle incoming Feishu message", error);
    });
  }
});

console.log("Starting DeepSeek Feishu bridge");
console.log(`Runtime: ${config.runtimeUrl}`);
console.log(`Workspace: ${config.workspace}`);
if (!config.allowlist.length && !config.allowUnlisted) {
  console.log("No allowlist configured. Incoming chats will receive their IDs and be refused.");
}

wsClient.start({ eventDispatcher: dispatcher });
void reattachActiveTurns().catch((error) => {
  console.error("failed to reattach active Feishu bridge turns", error);
});

async function handleIncomingMessage(event) {
  const identity = incomingIdentity(event);
  if (!identity.chatId || !identity.messageId) return;

  if (identity.messageType && identity.messageType !== "text") {
    await sendText(identity.chatId, "Only text messages are supported in this first bridge.");
    return;
  }

  const rawText = parseTextContent(event.message?.content || "");
  const scoped = stripGroupPrefix(rawText, {
    chatType: identity.chatType,
    requirePrefix: config.requirePrefixInGroup,
    prefix: config.groupPrefix
  });
  if (!scoped.accepted) return;

  await threadStore.handleMessageOnce(identity.messageId, async () => {
    if (identity.chatType !== "p2p" && !config.allowGroups) {
      await sendText(
        identity.chatId,
        "Group chat control is disabled for this bridge. DM the bot, or set FEISHU_ALLOW_GROUPS=true and allowlist this chat."
      );
      return;
    }

    if (!isAllowed(identity, config.allowlist, config.allowUnlisted)) {
      await sendText(identity.chatId, pairingRefusalText(identity));
      return;
    }

    // Only an admitted sender may change delivery/recovery provenance.
    const { chatId, chatType, openId, unionId, userId } = identity;
    await threadStore.patchChat(chatId, {
      authorizedIdentity: { chatId, chatType, openId, unionId, userId },
      replyToMessageId: identity.messageId,
      updatedAt: new Date().toISOString()
    });

    const command = parseCommand(scoped.text);
    await handleCommand(identity.chatId, command, identity);
  }, () => sendText(identity.chatId, "The bridge was interrupted while handling this message. Check /status before sending it again."));
}

async function handleCommand(chatId, command, identity) {
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
      const state = await ensureThread(chatId, { forceNew: true });
      await sendText(chatId, `Created thread ${state.threadId}`);
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
      await decideApproval(chatId, action, identity);
      return;
    case "set_model":
      await setChatModel(chatId, action.modelName);
      return;
    case "prompt":
      await runPrompt(chatId, action.prompt, identity);
      return;
    default:
      await sendText(chatId, helpText());
  }
}

async function ensureThread(chatId, { forceNew = false } = {}) {
  const existing = await threadStore.getChat(chatId);
  if (existing?.threadId && !forceNew) return existing;

  // Use per-chat model if set, fall back to bridge-level default.
  // / 优先使用 per-chat 模型（/model 命令设置），否则用桥接级别的默认模型。
  const effectiveModel = existing?.model || config.model;

  const thread = await runtimeJson("/v1/threads", {
    method: "POST",
    body: {
      model: effectiveModel,
      workspace: config.workspace,
      mode: config.mode,
      allow_shell: config.allowShell,
      trust_mode: config.trustMode,
      auto_approve: config.autoApprove,
      archived: false,
      system_prompt:
        "You are being controlled from a Feishu/Lark phone chat. Keep status updates concise. Ask for tool approvals when needed; do not assume mobile messages imply blanket approval."
    }
  });

  const state = {
    ...preservedChatStateFields(existing),
    threadId: thread.id,
    lastSeq: 0,
    activeTurnId: null,
    updatedAt: new Date().toISOString()
  };
  await threadStore.setChat(chatId, state);
  return state;
}

function turnActor(identity) {
  for (const kind of ["openId", "userId", "unionId"]) {
    if (identity?.[kind]) return `feishu-${kind}:${identity[kind]}`;
  }
  return "";
}

async function runPrompt(chatId, prompt, identity) {
  if (!prompt.trim()) {
    await sendText(chatId, helpText());
    return;
  }
  const actorId = turnActor(identity);
  if (!actorId) throw new ApprovalOwnershipError("turn needs its initiating human");
  const state = await ensureThread(chatId);
  // Use per-chat model for this turn (may differ from the thread's
  // creation model if the user ran /model after the thread was created).
  // / 使用 per-chat 模型执行本轮对话（如果用户在创建线程后切换过模型）。
  const effectiveModel = state?.model || config.model;
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}`);
  const activeBlock = activeTurnBlock(detail, state);
  if (activeBlock) {
    await threadStore.patchChat(chatId, {
      activeTurnId: activeBlock.turnId,
      updatedAt: new Date().toISOString()
    });
    await sendText(chatId, activeBlock.message);
    return;
  }
  if (state.activeTurnId) {
    await threadStore.patchChat(chatId, { activeTurnId: null });
  }
  const sinceSeq = Number(detail.latest_seq || state.lastSeq || 0);

  const turnResponse = await runtimeJson(
    `/v1/threads/${encodeURIComponent(state.threadId)}/turns`,
    {
      method: "POST",
      body: {
        prompt,
        input_summary: prompt.slice(0, 200),
        model: effectiveModel,
        mode: config.mode,
        allow_shell: config.allowShell,
        trust_mode: config.trustMode,
        auto_approve: config.autoApprove
      }
    }
  );

  const turnId = turnResponse.turn?.id;
  // A turn the Runtime accepted keeps streaming even when its origin cannot be
  // recorded; its approvals then fail closed and are decided from the TUI.
  if (turnId && actorId) {
    await threadStore.recordTurnOrigin(chatId, state.threadId, turnId, actorId);
  }
  await threadStore.patchChat(chatId, {
    activeTurnId: turnId || null,
    lastSeq: sinceSeq,
    updatedAt: new Date().toISOString()
  });
  await sendText(chatId, `Started turn ${turnId || "(unknown)"}`);

  if (await streamTurnEvents(chatId, state.threadId, turnId, sinceSeq)) {
    await threadStore.finishObservedTurn(chatId, state.threadId, turnId);
  }
}

async function reattachActiveTurns() {
  for (const [chatId, state] of threadStore.listChats()) {
    if (!state?.threadId || !state.activeTurnId) continue;
    const identity = state.authorizedIdentity;
    if (!identity || identity.chatId !== chatId ||
        !["p2p", "group"].includes(identity.chatType) ||
        (identity.chatType !== "p2p" && !config.allowGroups) ||
        !isAllowed(identity, config.allowlist, config.allowUnlisted)) continue;


    const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}`);
    const runningTurn = latestRunningTurn(detail);
    if (!runningTurn) {
      await threadStore.patchChat(chatId, {
        activeTurnId: null,
        lastSeq: Number(detail.latest_seq || state.lastSeq || 0),
        updatedAt: new Date().toISOString()
      });
      await sendText(chatId, `Bridge restarted. No active turn remains for ${state.threadId}.`);
      continue;
    }

    const turnId = runningTurn.id || state.activeTurnId;
    const sinceSeq = Number(state.lastSeq || 0);
    await threadStore.patchChat(chatId, {
      activeTurnId: turnId,
      updatedAt: new Date().toISOString()
    });
    await sendText(
      chatId,
      `Bridge restarted. Reattaching to active turn ${turnId} from seq ${sinceSeq}.`
    );
    if (await streamTurnEvents(chatId, state.threadId, turnId, sinceSeq)) {
      await threadStore.finishObservedTurn(chatId, state.threadId, turnId);
    }
  }
}

async function streamTurnEvents(chatId, threadId, turnId, sinceSeq) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), config.turnTimeoutMs);
  let responseText = "";
  let latestSeq = sinceSeq;
  let sentProgressAt = Date.now();

  try {
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

    for await (const record of readThreadEvents(response, threadId)) {
      if (record.event === "stream.end") return false;
      latestSeq = Math.max(latestSeq, Number(record.seq || 0));
      await threadStore.patchChat(chatId, { lastSeq: latestSeq });

      if (!turnId || record.turn_id !== turnId) continue;

      if (record.event === "item.delta" && record.payload?.kind === "agent_message") {
        responseText += record.payload.delta || "";
        const now = Date.now();
        if (responseText.length > config.maxReplyChars && now - sentProgressAt > 15000) {
          await sendText(chatId, responseText.slice(0, config.maxReplyChars));
          responseText = responseText.slice(config.maxReplyChars);
          sentProgressAt = now;
        }
      }

      if (record.event === "approval.required") {
        const approval = record.payload || {};
        await sendText(
          chatId,
          [
            "Approval required",
            `tool=${approval.tool_name || "unknown"}`,
            `approval_id=${approval.approval_id || approval.id}`,
            approval.description || "",
            "",
            `Reply /allow ${approval.approval_id || approval.id}`,
            `Reply /deny ${approval.approval_id || approval.id}`
          ]
            .filter(Boolean)
            .join("\n")
        );
      }

      if (record.event === "turn.completed") {
        const turn = record.payload?.turn || {};
        const status = turn.status || "completed";
        const error = turn.error ? `\n${turn.error}` : "";
        if (status !== "completed") {
          await sendText(chatId, `Turn ${status}.${error}`.trim());
        } else {
          await sendText(chatId, responseText.trim() || "Turn completed.");
        }
        return true;
      }

      if (record.event === "turn.lifecycle") {
        const status = record.payload?.turn?.status || record.payload?.status;
        if (["failed", "canceled", "interrupted"].includes(status)) {
          await sendText(chatId, `Turn ${status}.`);
          return true;
        }
      }
    }
  } catch (error) {
    if (error.name === "AbortError") {
      await sendText(chatId, `Turn observation timed out after ${Math.round(config.turnTimeoutMs / 1000)}s.`);
      return false;
    }
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

async function sendStatus(chatId) {
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
      `staged=${workspace.staged} unstaged=${workspace.unstaged} untracked=${workspace.untracked}`
    ]
      .filter(Boolean)
      .join("\n")
  );
}

async function sendThreads(chatId) {
  const threads = await runtimeJson("/v1/threads/summary?limit=8&include_archived=true");
  if (!threads.length) {
    await sendText(chatId, "No runtime threads yet.");
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
}

async function resumeThread(chatId, args) {
  const threadId = args.trim();
  if (!threadId) {
    await sendText(chatId, "Usage: /resume <thread_id>");
    return;
  }
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(threadId)}`);
  const existing = await threadStore.getChat(chatId);
  await threadStore.setChat(chatId, {
    ...preservedChatStateFields(existing),
    threadId,
    lastSeq: Number(detail.latest_seq || 0),
    activeTurnId: null,
    updatedAt: new Date().toISOString()
  });
  await sendText(chatId, `Resumed thread ${threadId}`);
}

async function interruptActiveTurn(chatId) {
  const state = await threadStore.getChat(chatId);
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
  await sendText(chatId, `Interrupt requested for ${turnId}`);
}

async function compactThread(chatId) {
  const state = await ensureThread(chatId);
  const result = await runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}/compact`, {
    method: "POST",
    body: { reason: "phone bridge request" }
  });
  await sendText(chatId, `Compaction started: ${result.turn?.id || "unknown turn"}`);
}

async function decideApproval(chatId, action, identity) {
  const decision = action.decision;
  const { approvalId, remember } =
    action.approvalId != null ? action : parseApprovalDecisionArgs(action.args);
  if (!approvalId) {
    await sendText(chatId, `Usage: /${decision} <approval_id>${decision === "allow" ? " [remember]" : ""}`);
    return;
  }
  try {
    await decideRuntimeApproval(runtimeJson, { store: threadStore, chatId, actorId: turnActor(identity), approvalId, decision, remember });
  } catch (error) {
    if (!(error instanceof ApprovalOwnershipError)) throw error;
    await sendText(chatId, `Approval ${approvalId} is not waiting in this chat.`);
    return;
  }
  await sendText(chatId, `Approval ${approvalId}: ${decision}${remember ? " and remember" : ""}`);
}

async function setChatModel(chatId, modelName) {
  // /model <name> — set per-chat model; "default" or empty resets to bridge default.
  // / /model "default" 或空参数 — 恢复桥接级别的默认模型。
  if (!modelName || modelName === "default") {
    await threadStore.patchChat(chatId, {
      model: null,
      updatedAt: new Date().toISOString()
    });
    await sendText(chatId, `Reset per-chat model. Using bridge default: ${config.model}`);
    return;
  }
  await threadStore.patchChat(chatId, {
    model: modelName,
    updatedAt: new Date().toISOString()
  });
  await sendText(chatId, `Per-chat model set to: ${modelName}`);
}

async function sendText(chatId, text) {
  // Try reply API first — keeps bot responses inside the same Feishu
  // thread/topic instead of spawning new standalone topics.
  // / 优先使用 reply API，确保 bot 回复留在话题群的同一条话题内。
  const state = await threadStore.getChat(chatId);
  const replyToMessageId = state?.replyToMessageId || null;

  const replyMessage =
    replyToMessageId
      ? client.im?.v1?.message?.reply?.bind(client.im.v1.message) ||
        client.im?.message?.reply?.bind(client.im.message)
      : null;
  const createMessage =
    client.im?.v1?.message?.create?.bind(client.im.v1.message) ||
    client.im?.message?.create?.bind(client.im.message);
  if (!createMessage) {
    throw new Error("Lark SDK client does not expose im message create API");
  }

  let canReply = Boolean(replyMessage);
  for (const chunk of splitMessage(text, config.maxReplyChars)) {
    const body = {
      msg_type: "text",
      content: JSON.stringify({ text: chunk })
    };
    if (canReply) {
      try {
        await replyMessage({
          path: { message_id: replyToMessageId },
          data: body
        });
        continue;
      } catch (error) {
        canReply = false;
        console.warn("Feishu reply API failed; falling back to message create", error);
      }
    }
    await createMessage({
      params: { receive_id_type: "chat_id" },
      data: { ...body, receive_id: chatId }
    });
  }
}

function requiredEnv(name) {
  const value = process.env[name];
  if (!value || !value.trim()) {
    throw new Error(`${name} is required`);
  }
  return value.trim();
}

function resolveLarkDomain(domain) {
  const normalized = String(domain || "feishu").toLowerCase();
  if (normalized === "lark") return Lark.Domain?.Lark || "https://open.larksuite.com";
  if (normalized === "feishu") return Lark.Domain?.Feishu || "https://open.feishu.cn";
  return domain;
}
