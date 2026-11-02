import { WSClient, generateReqId } from "@wecom/aibot-node-sdk";

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
  isApprovalResponse,
  isDenyResponse,
  preservedChatStateFields,
  requiredEnv,
  splitMessage,
  stripGroupPrefix,
  ThreadStore
} from "./lib.mjs";
import {
  ApprovalOwnershipError,
  createRuntimeClient,
  decideApproval as decideRuntimeApproval,
  readJsonSafe,
  readThreadEvents
} from "../../bridge-core/src/lib.mjs";

/** Map of chatId -> latest pending approval info for natural-language approval. */
const pendingApprovals = new Map();
// Clean up stale approvals every 2 minutes
setInterval(() => {
  const now = Date.now();
  for (const [chatId, approval] of pendingApprovals) {
    if (now - approval.timestamp > 300_000) pendingApprovals.delete(chatId);
  }
}, 120_000);

const config = {
  botId: requiredEnv("WECOM_BOT_ID"),
  botSecret: requiredEnv("WECOM_BOT_SECRET"),
  runtimeUrl: (process.env.CODEWHALE_RUNTIME_URL || "http://127.0.0.1:7878").replace(/\/+$/, ""),
  runtimeToken: requiredEnv("CODEWHALE_RUNTIME_TOKEN"),
  workspace: process.env.CODEWHALE_WORKSPACE || process.cwd(),
  model: process.env.CODEWHALE_MODEL || "auto",
  mode: process.env.CODEWHALE_MODE || "agent",
  allowShell: parseBool(process.env.CODEWHALE_ALLOW_SHELL, true),
  trustMode: parseBool(process.env.CODEWHALE_TRUST_MODE, false),
  autoApprove: parseBool(process.env.CODEWHALE_AUTO_APPROVE, false),
  allowlist: parseList(process.env.WECOM_CHAT_ALLOWLIST),
  allowUnlisted: parseBool(process.env.WECOM_ALLOW_UNLISTED, false),
  threadMapPath: process.env.WECOM_THREAD_MAP_PATH || "/var/lib/codewhale-wecom-bridge/thread-map.json",
  maxReplyChars: Number(process.env.WECOM_MAX_REPLY_CHARS || 3500),
  turnTimeoutMs: Number(process.env.CODEWHALE_TURN_TIMEOUT_MS || 900000),
  approvalTimeoutMs: Number(process.env.CODEWHALE_APPROVAL_TIMEOUT_MS || 300000)
};

const { runtimeJson, authHeaders } = createRuntimeClient(config);

const threadStore = await ThreadStore.open(config.threadMapPath);

const client = new WSClient({
  botId: config.botId,
  secret: config.botSecret
});

client.on("message", async (frame) => {
  try {
    await handleIncomingMessage(frame);
  } catch (error) {
    await reportHandlerError(frame, "Failed to handle WeCom message", error);
  }
});

client.on("event", async (frame) => {
  try {
    await handleEvent(frame);
  } catch (error) {
    await reportHandlerError(frame, "Failed to handle WeCom event", error);
  }
});

client.on("error", (error) => {
  console.error("WeCom client error:", error);
});

console.log("Starting CodeWhale WeCom bridge");
console.log(`Runtime: ${config.runtimeUrl}`);
console.log(`Workspace: ${config.workspace}`);
if (!config.allowlist.length && !config.allowUnlisted) {
  console.log("No allowlist configured. Incoming chats will receive their IDs and be refused.");
}

client.connect();

function replyText(frame, text) {
  const chunks = splitMessage(text, config.maxReplyChars);
  return chunks.reduce(
    (chain, chunk) => chain.then(() => client.replyStream(frame, generateReqId("stream"), chunk, true)),
    Promise.resolve()
  );
}

async function reportHandlerError(frame, context, error) {
  console.error(context, error);
  try {
    await replyText(frame, `${context}: ${publicBridgeError(error)}`);
  } catch (replyError) {
    console.error("Failed to report WeCom bridge error", replyError);
  }
}

function publicBridgeError(error) {
  const message = String(error?.message || error || "unknown error");
  return message.replaceAll(config.runtimeToken, "<redacted>").slice(0, 500);
}

async function handleIncomingMessage(frame) {
  const body = frame.body || {};
  const identity = incomingIdentity(body);
  console.log(`Incoming message: chatId=${identity.chatId} userId=${identity.userId} chatType=${identity.chatType}`);
  if (!identity.chatId || !identity.messageId) return;

  if (body.msgtype && body.msgtype !== "text") {
    await replyText(frame, "目前仅支持文本消息。");
    return;
  }

  const textContent = body.text?.content || "";
  const scoped = stripGroupPrefix(textContent, {
    chatType: identity.chatType,
    requirePrefix: identity.chatType === "group",
    prefix: "/ds"
  });
  if (!scoped.accepted) return;

  if (!isAllowed(identity, config.allowlist, config.allowUnlisted)) {
    await replyText(frame, pairingRefusalText(identity));
    return;
  }

  const command = parseCommand(scoped.text);
  await handleCommand(identity.chatId, command, frame);
}

async function handleEvent(frame) {
  const body = frame.body || {};
  const eventType = body.event?.eventtype || "";
  if (eventType === "enter_chat") {
    const chatId = body.chatid;
    if (chatId) {
      await client.replyWelcome(frame, { msgtype: "text", text: { content: "欢迎使用 CodeWhale！发送 /help 查看可用命令。" } });
    }
  }
}

async function handleCommand(chatId, command, frame) {
  const action = commandAction(command);
  switch (action.kind) {
    case "help":
      await replyText(frame, helpText());
      return;
    case "status":
      await sendStatus(chatId, frame);
      return;
    case "threads":
      await sendThreads(chatId, frame);
      return;
    case "new_thread": {
      const state = await ensureThread(chatId);
      await replyText(frame, `Created thread ${state.threadId}`);
      return;
    }
    case "resume":
      await resumeThread(chatId, action.threadId, frame);
      return;
    case "interrupt":
      await interruptActiveTurn(chatId, frame);
      return;
    case "compact":
      await compactThread(chatId, frame);
      return;
    case "approval":
      await decideApproval(chatId, action, frame);
      return;
    case "set_model":
      await setChatModel(chatId, action.modelName, frame);
      return;
    case "prompt":
      // Check if this is a natural-language approval/deny response
      // This map supplies an approval id for convenience only. Runtime's
      // pending turn and its durable initiating human still decide authority.
      if (pendingApprovals.has(chatId)) {
        const pending = pendingApprovals.get(chatId);
        if (Date.now() - pending.timestamp < config.approvalTimeoutMs) {
          if (isApprovalResponse(action.prompt)) {
            const action2 = { kind: "approval", decision: "allow", approvalId: pending.approvalId };
            if (await decideApproval(chatId, action2, frame)) pendingApprovals.delete(chatId);
            return;
          }
          if (isDenyResponse(action.prompt)) {
            const action2 = { kind: "approval", decision: "deny", approvalId: pending.approvalId };
            if (await decideApproval(chatId, action2, frame)) pendingApprovals.delete(chatId);
            return;
          }
        }
      }
      await runPrompt(chatId, action.prompt, frame);
      return;
    default:
      await replyText(frame, helpText());
  }
}

async function ensureThread(chatId, { forceNew = false } = {}) {
  const existing = await threadStore.getChat(chatId);
  if (existing?.threadId && !forceNew) return existing;

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
        "You are being controlled from a WeCom (企业微信) phone chat. Keep status updates concise. Ask for tool approvals when needed; do not assume mobile messages imply blanket approval."
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
  return identity?.userId ? `wecom-user:${identity.userId}` : "";
}

async function runPrompt(chatId, prompt, frame) {
  if (!prompt.trim()) {
    await replyText(frame, helpText());
    return;
  }
  const actorId = turnActor(incomingIdentity(frame.body));
  if (!actorId) throw new ApprovalOwnershipError("turn needs its initiating human");
  const state = await ensureThread(chatId);
  const effectiveModel = state?.model || config.model;
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}`);
  const activeBlock = activeTurnBlock(detail, state);
  if (activeBlock) {
    await threadStore.patchChat(chatId, {
      activeTurnId: activeBlock.turnId,
      updatedAt: new Date().toISOString()
    });
    await replyText(frame, activeBlock.message);
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
  await replyText(frame, `Started turn ${turnId || "(unknown)"}`);

  if (await streamTurnEvents(chatId, frame, state.threadId, turnId, sinceSeq)) {
    await threadStore.finishObservedTurn(chatId, state.threadId, turnId);
  }
}

async function streamTurnEvents(chatId, frame, threadId, turnId, sinceSeq) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), config.turnTimeoutMs);
  const streamId = generateReqId("stream");
  let responseText = "";
  let latestSeq = sinceSeq;

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
        await client.replyStream(frame, streamId, responseText, false);
      }

      if (record.event === "approval.required") {
        const approval = record.payload || {};
        const approvalId = approval.approval_id || approval.id;
        // Track latest pending approval per chat for natural-language responses
        if (approvalId) {
          pendingApprovals.set(chatId, {
            approvalId,
            toolName: approval.tool_name || "unknown",
            description: approval.description || "",
            timestamp: Date.now()
          });
        }
        await replyText(
          frame,
          [
            "审批请求",
            `tool=${approval.tool_name || "unknown"}`,
            `approval_id=${approvalId}`,
            approval.description || "",
            "",
            `回复 /allow ${approvalId}`,
            `回复 /deny ${approvalId}`,
            "也可以直接回复「允许」或「拒绝」"
          ]
            .filter(Boolean)
            .join("\n")
        );
      }

      if (record.event === "turn.completed") {
        const turn = record.payload?.turn || {};
        const status = turn.status || "completed";
        const errorText = turn.error ? `\n${turn.error}` : "";
        const fallback = status === "completed" ? "Turn completed." : `Turn ${status}.${errorText}`;
        await client.replyStream(frame, streamId, responseText.trim() || fallback, true);
        return true;
      }

      if (record.event === "turn.lifecycle") {
        const turn = record.payload?.turn || {};
        const status = turn.status || record.payload?.status;
        if (["failed", "canceled", "interrupted"].includes(status)) {
          const errorText = turn.error || record.payload?.error;
          await client.replyStream(frame, streamId, `Turn ${status}.${errorText ? `\n${errorText}` : ""}`, true);
          return true;
        }
      }
    }
  } catch (error) {
    if (error.name === "AbortError") {
      await replyText(frame, `Turn observation timed out after ${Math.round(config.turnTimeoutMs / 1000)}s.`);
      return false;
    }
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

async function sendStatus(chatId, frame) {
  const [health, runtimeInfo, workspace] = await Promise.all([
    runtimeJson("/health", { auth: false }),
    runtimeJson("/v1/runtime/info"),
    runtimeJson("/v1/workspace/status")
  ]);
  await replyText(
    frame,
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

async function sendThreads(chatId, frame) {
  const threads = await runtimeJson("/v1/threads/summary?limit=8&include_archived=true");
  if (!threads.length) {
    await replyText(frame, "No runtime threads yet.");
    return;
  }
  await replyText(
    frame,
    threads
      .map((thread) => {
        const status = thread.latest_turn_status || "none";
        return `${thread.id} [${status}] ${thread.title || thread.preview || ""}`;
      })
      .join("\n")
  );
}

async function resumeThread(chatId, args, frame) {
  const threadId = args.trim();
  if (!threadId) {
    await replyText(frame, "Usage: /resume <thread_id>");
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
  await replyText(frame, `Resumed thread ${threadId}`);
}

async function interruptActiveTurn(chatId, frame) {
  const state = await threadStore.getChat(chatId);
  if (!state?.threadId) {
    await replyText(frame, "No runtime thread recorded for this chat.");
    return;
  }
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}`);
  const runningTurn = latestRunningTurn(detail);
  const turnId = state.activeTurnId || runningTurn?.id;
  if (!turnId) {
    await replyText(frame, "No active turn recorded for this chat.");
    return;
  }
  await runtimeJson(
    `/v1/threads/${encodeURIComponent(state.threadId)}/turns/${encodeURIComponent(turnId)}/interrupt`,
    { method: "POST" }
  );
  await threadStore.patchChat(chatId, {
    activeTurnId: turnId,
    updatedAt: new Date().toISOString()
  });
  await replyText(frame, `Interrupt requested for ${turnId}`);
}

async function compactThread(chatId, frame) {
  const state = await ensureThread(chatId);
  const result = await runtimeJson(`/v1/threads/${encodeURIComponent(state.threadId)}/compact`, {
    method: "POST",
    body: { reason: "phone bridge request" }
  });
  await replyText(frame, `Compaction started: ${result.turn?.id || "unknown turn"}`);
}

async function decideApproval(chatId, action, frame) {
  const decision = action.decision;
  // WeCom approvals are text: a session-wide "remember" is never taken from
  // a chat message.
  const { approvalId } =
    action.approvalId != null ? action : parseApprovalDecisionArgs(action.args);
  const remember = false;
  if (!approvalId) {
    await replyText(frame, `Usage: /${decision} <approval_id>`);
    return false;
  }
  try {
    await decideRuntimeApproval(runtimeJson, { store: threadStore, chatId, actorId: turnActor(incomingIdentity(frame.body)), approvalId, decision, remember });
  } catch (error) {
    if (!(error instanceof ApprovalOwnershipError)) throw error;
    await replyText(frame, `Approval ${approvalId} is not waiting in this chat.`);
    return false;
  }

  // Clear activeTurnId so the user can send follow-up messages
  // immediately instead of being blocked by activeTurnBlock
  // while the SSE stream processes the turn cancellation.
  await threadStore.patchChat(chatId, {
    activeTurnId: null,
    updatedAt: new Date().toISOString()
  });

  await replyText(frame, `Approval ${approvalId}: ${decision}${remember ? " and remember" : ""}`);
  return true;
}

async function setChatModel(chatId, modelName, frame) {
  if (!modelName || modelName === "default") {
    await threadStore.patchChat(chatId, {
      model: null,
      updatedAt: new Date().toISOString()
    });
    await replyText(frame, `Reset per-chat model. Using bridge default: ${config.model}`);
    return;
  }
  await threadStore.patchChat(chatId, {
    model: modelName,
    updatedAt: new Date().toISOString()
  });
  await replyText(frame, `Per-chat model set to: ${modelName}`);
}
