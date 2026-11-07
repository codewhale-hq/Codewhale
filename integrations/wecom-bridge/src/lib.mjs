import {
  activeTurnBlock,
  cleanEnvValue,
  commandAction as coreCommandAction,
  compactRuntimeError,
  isPlaceholderValue,
  latestRunningTurn,
  parseApprovalDecisionArgs,
  parseBool,
  parseCommand,
  parseList,
  parseTextContent as coreParseTextContent,
  preservedChatStateFields,
  splitMessage,
  stripGroupPrefix as coreStripGroupPrefix,
  ThreadStore as CoreThreadStore
} from "../../bridge-core/src/lib.mjs";

export {
  activeTurnBlock,
  cleanEnvValue,
  compactRuntimeError,
  isPlaceholderValue,
  latestRunningTurn,
  parseApprovalDecisionArgs,
  parseBool,
  parseCommand,
  parseList,
  preservedChatStateFields,
  splitMessage
};

export function requiredEnv(name) {
  const value = process.env[name];
  if (!value || !value.trim()) {
    throw new Error(`${name} is required`);
  }
  return value.trim();
}

export function parseTextContent(content) {
  return coreParseTextContent(content, ["text"]);
}

export function incomingIdentity(body) {
  const from = body?.from || {};
  const chatId = body.chatid || (body.chattype === "single" && from.userid ? `single:${from.userid}` : "");
  return {
    chatId,
    messageId: body.msgid || "",
    chatType: body.chattype || "single",
    userId: from.userid || "",
    aibotId: body.aibotid || ""
  };
}

export function isAllowed(identity, allowlist, allowUnlisted = false) {
  if (allowUnlisted) return true;
  const allowed = new Set(allowlist);
  return [identity.chatId, identity.userId].filter(Boolean).some((id) => allowed.has(id));
}

export function pairingRefusalText(identity) {
  return [
    "This chat is not in WECOM_CHAT_ALLOWLIST.",
    `chat_id=${identity.chatId}`,
    identity.userId ? `user_id=${identity.userId}` : ""
  ]
    .filter(Boolean)
    .join("\n");
}

export function stripGroupPrefix(text, { chatType, requirePrefix, prefix }) {
  return coreStripGroupPrefix(text, {
    chatType,
    requirePrefix,
    prefix: prefix || "/ds",
    directChatTypes: ["single"]
  });
}

/** Reduce a provider-supplied filename to a safe single path component. */
export function sanitizeFilename(name, fallback = "upload.bin") {
  const cleaned = String(name || "")
    .replace(/[\\/]/g, "_")
    // eslint-disable-next-line no-control-regex
    .replace(/[\u0000-\u001f]/g, "")
    .replace(/^[.\s]+/, "")
    .replace(/[\s.]+$/, "")
    .slice(0, 120);
  return cleaned || fallback;
}

/**
 * Render an error for a chat reply. WeCom SDK rejections arrive as plain
 * objects ({errcode, errmsg}); String() on those yields "[object Object]".
 */
export function formatPublicError(error) {
  if (error && typeof error === "object") {
    if (error.errmsg != null || error.errcode != null) {
      return `errcode=${error.errcode}: ${error.errmsg || ""}`.trim();
    }
    if (error.message) return String(error.message);
    try { return JSON.stringify(error).slice(0, 300); } catch { return "unknown error"; }
  }
  return String(error || "unknown error");
}

/**
 * Wrap client.replyStream with a WeCom stream lifetime policy. WeCom stream
 * messages stop accepting updates after ~10 minutes (errcode 846608), so a
 * long-running turn outlives its stream and every later update fails. This
 * helper finalizes the current stream and opens a fresh one when it nears
 * the cap, and reopens once on a rejected update.
 */
export function createStreamReply(client, frame, { rotateMs = 8 * 60_000, now = Date.now, genId } = {}) {
  let streamId = genId ? genId() : `stream-${Math.random().toString(36).slice(2)}`;
  let openedAt = now();
  async function send(id, text, finish) {
    await client.replyStream(frame, id, text, finish);
  }
  return {
    get streamId() { return streamId; },
    async update(text, finish = false) {
      if (!finish && now() - openedAt > rotateMs) {
        try { await send(streamId, `${text}\n…(continued)`, true); } catch { /* expired already */ }
        streamId = genId ? genId() : `stream-${Math.random().toString(36).slice(2)}`;
        openedAt = now();
      }
      try {
        await send(streamId, text, finish);
      } catch {
        streamId = genId ? genId() : `stream-${Math.random().toString(36).slice(2)}`;
        openedAt = now();
        try { await send(streamId, text, finish); }
        catch (e) { console.error("stream update failed twice", e?.errmsg || e?.message || e); }
      }
    }
  };
}

/** Check if text is a natural-language approval response (Chinese or English). */
export function isApprovalResponse(text) {
  const t = String(text || "").trim().toLowerCase();
  // Single-word approvals
  if (["允许", "可以", "好", "同意", "批准", "yes", "ok", "y", "approve", "allow"].includes(t)) return true;
  // Two-char approvals
  if (["好的", "可以", "没问题", "批准了", "同意"].includes(t)) return true;
  return false;
}

/** Check if text is a natural-language deny response. */
export function isDenyResponse(text) {
  const t = String(text || "").trim().toLowerCase();
  if (["拒绝", "不行", "不要", "no", "n", "deny", "reject", "取消", "stop", "否"].includes(t)) return true;
  if (["不可以", "不同意", "不要执行"].includes(t)) return true;
  return false;
}

export function commandAction(command) {
  return coreCommandAction(command);
}

export function helpText() {
  return [
    "CodeWhale 企业微信桥接命令:",
    "/help - 显示帮助",
    "/status - runtime 和工作区状态",
    "/threads - 最近的 runtime 线程",
    "/new - 为此聊天创建新线程",
    "/resume <thread_id> - 绑定到此聊天的现有线程",
    "/model <name|default> - 设置或重置聊天模型",
    "/interrupt - 中断活动 turn",
    "/compact - 压缩当前线程",
    "/allow <approval_id> - 批准待处理的工具调用",
    "/deny <approval_id> - 拒绝待处理的工具调用",
    "",
    "其他所有内容均作为 CodeWhale 提示发送。"
  ].join("\n");
}

export class ThreadStore extends CoreThreadStore {
  constructor(filePath) {
    super(filePath, { privateMode: true });
  }
}

export function validateBridgeConfig(env) {
  const errors = [];
  const warnings = [];
  const info = [];
  const add = (list, code, message) => list.push({ code, message });

  for (const key of ["WECOM_BOT_ID", "WECOM_BOT_SECRET"]) {
    const value = cleanEnvValue(env[key]);
    if (!value) {
      add(errors, "missing_required", `${key} is required`);
    } else if (isPlaceholderValue(value)) {
      add(errors, "placeholder_value", `${key} still contains a placeholder value`);
    }
  }

  const runtimeUrl = cleanEnvValue(env.CODEWHALE_RUNTIME_URL || "http://127.0.0.1:7878");
  try {
    const parsed = new URL(runtimeUrl);
    if (!["http:", "https:"].includes(parsed.protocol)) {
      add(errors, "invalid_runtime_url", "CODEWHALE_RUNTIME_URL must use http or https");
    }
  } catch {
    add(errors, "invalid_runtime_url", "CODEWHALE_RUNTIME_URL is not a valid URL");
  }

  const runtimeToken = cleanEnvValue(env.CODEWHALE_RUNTIME_TOKEN);
  if (!runtimeToken) {
    add(errors, "missing_runtime_token", "CODEWHALE_RUNTIME_TOKEN is required");
  } else if (isPlaceholderValue(runtimeToken)) {
    add(errors, "placeholder_runtime_token", "CODEWHALE_RUNTIME_TOKEN is still a placeholder");
  }

  const allowUnlisted = parseBool(env.WECOM_ALLOW_UNLISTED, false);
  const allowlist = parseList(env.WECOM_CHAT_ALLOWLIST);

  if (!allowlist.length && allowUnlisted) {
    add(warnings, "pairing_mode_open", "WECOM_ALLOW_UNLISTED=true leaves first-pairing mode open");
  } else if (!allowlist.length) {
    add(warnings, "not_paired", "WECOM_CHAT_ALLOWLIST is empty; all chats will be refused");
  }

  return { ok: errors.length === 0, errors, warnings, info };
}

export function formatValidationReport(result) {
  const lines = ["WeCom bridge config validation"];
  for (const item of result.errors) lines.push(`[fail] ${item.message}`);
  for (const item of result.warnings) lines.push(`[warn] ${item.message}`);
  for (const item of result.info) lines.push(`[info] ${item.message}`);
  if (result.ok) lines.push("[ok] No blocking config errors found");
  return lines.join("\n");
}
