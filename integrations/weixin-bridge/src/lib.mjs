import crypto from "node:crypto";

import {
  activeTurnBlock,
  commandAction,
  compactRuntimeError,
  envFirst,
  latestRunningTurn,
  parseApprovalDecisionArgs,
  parseBool,
  parseCommand,
  parseList,
  preservedChatStateFields,
  splitMessage,
} from "../../bridge-core/src/lib.mjs";

// ============================================================================
// iLink Bot API 协议层 — 参考 @tencent-weixin/openclaw-weixin
// ============================================================================

const DEFAULT_API_TIMEOUT_MS = 30_000;
const LONGPOLL_DEFAULT_TIMEOUT_MS = 35_000;

export {
  activeTurnBlock,
  commandAction,
  compactRuntimeError,
  envFirst,
  latestRunningTurn,
  parseApprovalDecisionArgs,
  parseBool,
  parseCommand,
  parseList,
  preservedChatStateFields,
  splitMessage,
};

/**
 * Handle one getUpdates batch, then advance the poll cursor. The cursor moves
 * only after every message in the batch was handled, so a crash replays the
 * batch from iLink instead of dropping a prompt. Each message is claimed
 * durably first: on replay, finished messages are skipped, and one a crashed
 * process was still handling is reported through `interrupted` rather than
 * run twice (its effect is unknown).
 */
export async function processUpdateBatch({ messages, nextCursor, store, keyOf, handle, interrupted, commitCursor }) {
  for (const msg of messages || []) {
    const key = keyOf(msg);
    if (!key) continue;
    await store.handleMessageOnce(key, () => handle(msg), () => interrupted(msg));
  }
  if (nextCursor) await commitCursor(nextCursor);
}

export function randomUin() {
  const uint32 = crypto.randomBytes(4).readUInt32BE(0);
  return Buffer.from(String(uint32), "utf-8").toString("base64");
}

// iLink-App-Id: 从 openclaw-weixin 的 package.json 已知为 "bot"
const ILINK_APP_ID = "bot";

// iLink-App-ClientVersion: 0x00MMNNPP (major<<16 | minor<<8 | patch)
function buildClientVersion(version) {
  const [major = 0, minor = 0, patch = 0] = version.split(".").map(Number);
  return ((major & 0xff) << 16) | ((minor & 0xff) << 8) | (patch & 0xff);
}
const ILINK_APP_CLIENT_VERSION = String(buildClientVersion("2.4.4"));

// ---------------------------------------------------------------------------
// 消息提取
// ---------------------------------------------------------------------------

export const MessageItemType = {
  NONE: 0,
  TEXT: 1,
  IMAGE: 2,
  VOICE: 3,
  FILE: 4,
  VIDEO: 5,
  TOOL_CALL_START: 11,
  TOOL_CALL_RESULT: 12,
};

export function extractText(itemList) {
  if (!Array.isArray(itemList) || !itemList.length) return "";
  for (const item of itemList) {
    if (item.type === MessageItemType.TEXT && item.text_item?.text != null) {
      return String(item.text_item.text);
    }
    if (item.type === MessageItemType.VOICE && item.voice_item?.text) {
      return item.voice_item.text;
    }
  }
  return "";
}

// ---------------------------------------------------------------------------
// 帮助文本
// ---------------------------------------------------------------------------

export function helpText() {
  return [
    "CodeWhale Weixin Bot 命令:",
    "/help (/帮助) - 显示此帮助",
    "/status (/状态) - 运行时和工作区状态，并重新发出待批准的批准码",
    "/threads (/对话) - 最近运行时线程",
    "/new (/新对话) - 为此聊天创建新线程",
    "/resume <thread_id> - 绑定到已有线程",
    "/model <name|default> - 设置或重置此聊天的模型",
    "/stop (/interrupt, /停止) - 中断活跃 turn",
    "/compact - 压缩当前线程",
    "1 <批准码> / 2 <批准码> - 允许或拒绝工具调用",
    "/allow <approval_id> [remember] - 批准工具调用",
    "/deny <approval_id> - 拒绝工具调用",
    "继续 - 查看剩余的回复",
    "",
    "其他所有内容均作为 CodeWhale 提示发送。",
  ].join("\n");
}

const COMMAND_ALIASES = { 帮助: "help", 状态: "status", 对话: "threads", 新对话: "new", 停止: "interrupt", stop: "interrupt" };

export function parseWeixinCommand(text) {
  const command = parseCommand(text);
  const alias = COMMAND_ALIASES[command.name];
  return alias ? { ...command, name: alias } : command;
}

const CONTINUE_REPLIES = new Set(["继续", "more", "continue"]);

export function isContinueReply(text) {
  return CONTINUE_REPLIES.has(String(text || "").trim().toLowerCase());
}

const CODE_ALPHABET = "23456789abcdefghjkmnpqrstuvwxyz";
const APPROVAL_REPLY = /^(1|2|y|n|yes|no|允许|同意|拒绝)\s+([23456789a-hjkmnp-z]{6})$/u;
const ALLOW_WORDS = new Set(["1", "y", "yes", "允许", "同意"]);

export function mintCode(length = 6) {
  return Array.from(crypto.randomBytes(length), (byte) => CODE_ALPHABET[byte % CODE_ALPHABET.length]).join("");
}

export function parseApprovalReply(text) {
  const match = String(text || "").trim().toLowerCase().match(APPROVAL_REPLY);
  if (!match) return null;
  return { decision: ALLOW_WORDS.has(match[1]) ? "allow" : "deny", code: match[2] };
}

function clip(text, max) {
  const value = String(text || "").trim();
  return value.length > max ? `${value.slice(0, max - 1)}…` : value;
}

export function approvalText(approval, code) {
  const what = [approval.tool_name, approval.intent_summary || approval.summary || approval.description].filter(Boolean).join(": ");
  return [
    "需要你的批准 / Approval needed",
    clip(what, 400),
    `回复 1 ${code} 允许，2 ${code} 拒绝`,
    `Reply 1 ${code} to allow, 2 ${code} to deny`,
  ].filter(Boolean).join("\n");
}

export { questionText, parseQuestionAnswers } from "../../bridge-core/src/lib.mjs";

export function isSessionExpired(response) {
  return response?.ret === -14 || response?.errcode === -14;
}

// ============================================================================
// iLink Bot HTTP API 调用
// ============================================================================

export const ILinkLoginBase = "https://ilinkai.weixin.qq.com";

function authHeaders({ token } = {}) {
  const headers = {
    "Content-Type": "application/json",
    AuthorizationType: "ilink_bot_token",
    "X-WECHAT-UIN": randomUin(),
    "iLink-App-Id": ILINK_APP_ID,
    "iLink-App-ClientVersion": ILINK_APP_CLIENT_VERSION,
  };
  if (token) {
    headers["Authorization"] = `Bearer ${token}`;
  }
  return headers;
}

/**
 * 通用 POST 到 iLink API。
 */
export async function apiPost({ baseUrl, endpoint, body, token, timeoutMs, signal }) {
  signal?.throwIfAborted();
  const url = `${baseUrl.replace(/\/+$/, "")}/${endpoint}`;
  const ms = timeoutMs || DEFAULT_API_TIMEOUT_MS;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), ms);
  const onAbort = () => controller.abort(signal.reason);
  signal?.addEventListener("abort", onAbort, { once: true });

  try {
    const response = await fetch(url, {
      method: "POST",
      headers: authHeaders({ token }),
      body,
      signal: controller.signal,
    });
    const text = await response.text();
    if (!response.ok) {
      throw new Error(
        `iLink API ${endpoint} failed: HTTP ${response.status}`
      );
    }
    return text;
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener("abort", onAbort);
  }
}

/**
 * 通用 GET 到 iLink API（用于轮询扫码状态等）。
 */
export async function apiGet({ baseUrl, endpoint, token, timeoutMs, signal }) {
  const url = `${baseUrl.replace(/\/+$/, "")}/${endpoint}`;
  const ms = timeoutMs || DEFAULT_API_TIMEOUT_MS;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), ms);
  signal?.addEventListener("abort", () => controller.abort(), { once: true });

  try {
    const response = await fetch(url, {
      method: "GET",
      headers: authHeaders({ token }),
      signal: controller.signal,
    });
    const text = await response.text();
    if (!response.ok) {
      // 长轮询超时是正常的，不抛错
      if (response.status === 524 || text.includes("timeout")) {
        return text;
      }
      throw new Error(
        `iLink API ${endpoint} failed: HTTP ${response.status} — ${text.slice(0, 200)}`
      );
    }
    return text;
  } finally {
    clearTimeout(timer);
  }
}

// ============================================================================
// 扫码登录
// ============================================================================

/**
 * 获取登录二维码。
 * 参考 login-qr.ts:79-90 — 端点 get_bot_qrcode，bot_type 是 query 参数。
 * @returns {Promise<{qrcode: string, qrcodeUrl: string, sessionKey: string}>}
 */
export async function getLoginQR({ botType = "3" } = {}) {
  const raw = await apiPost({
    baseUrl: ILinkLoginBase,
    endpoint: `ilink/bot/get_bot_qrcode?bot_type=${encodeURIComponent(botType)}`,
    body: JSON.stringify({ local_token_list: [] }),
  });
  const data = JSON.parse(raw);
  const qrcodeUrl = data.qrcode_img_content || "";
  const sessionKey = data.qrcode || crypto.randomUUID();
  return { qrcode: data.qrcode, qrcodeUrl, sessionKey };
}

/**
 * 轮询扫码状态直到确认或超时。
 * 参考 login-qr.ts:112-136 — 端点 get_qrcode_status，GET 方法。
 * @returns {Promise<{connected: boolean, botToken?: string, accountId?: string, baseUrl?: string, userId?: string, message: string}>}
 */
export async function waitForLogin({ sessionKey, timeoutMs = 300_000 } = {}) {
  const deadline = Date.now() + timeoutMs;

  while (Date.now() < deadline) {
    let raw;
    try {
      raw = await apiGet({
        baseUrl: ILinkLoginBase,
        endpoint: `ilink/bot/get_qrcode_status?qrcode=${encodeURIComponent(sessionKey)}`,
        timeoutMs: 35_000, // 长轮询超时
      });
    } catch {
      // 超时等网络错误 → 视为 wait，继续轮询
      await new Promise((r) => setTimeout(r, 1000));
      continue;
    }

    const data = JSON.parse(raw);
    const status = data.status;

    if (status === "confirmed") {
      return {
        connected: true,
        botToken: data.bot_token,
        accountId: data.ilink_bot_id,
        baseUrl: data.baseurl || ILinkLoginBase,
        userId: data.ilink_user_id,
        message: "已连接微信。",
      };
    }

    if (status === "expired" || status === "binded_redirect") {
      return { connected: false, message: status === "expired" ? "二维码已过期，请重试。" : "已连接过此桥接，无需重复连接。" };
    }

    // 等 1 秒再轮询
    await new Promise((r) => setTimeout(r, 1000));
  }

  return { connected: false, message: "登录超时，请重试。" };
}

// ============================================================================
// 消息 API
// ============================================================================

/**
 * 长轮询获取新消息。
 */
export async function getUpdates({ baseUrl, token, get_updates_buf = "", timeoutMs = LONGPOLL_DEFAULT_TIMEOUT_MS, signal }) {
  const raw = await apiPost({
    baseUrl,
    endpoint: "ilink/bot/getupdates",
    body: JSON.stringify({
      get_updates_buf,
      base_info: { bot_agent: "CodeWhale/1.0" },
    }),
    token,
    timeoutMs,
    signal,
  });
  return JSON.parse(raw);
}

/**
 * Send a message and return the parsed API acceptance response, not a recipient
 * delivery receipt. Tencent's reference allows omitted status fields; explicit
 * statuses must be numeric zero. Ambiguous failures must not be retried as if
 * the provider had rejected the message.
 */
export async function sendMessage({ baseUrl, token, body, timeoutMs }) {
  let raw;
  try {
    raw = await apiPost({
      baseUrl,
      endpoint: "ilink/bot/sendmessage",
      body: JSON.stringify({
        ...body,
        base_info: { bot_agent: "CodeWhale/1.0" },
      }),
      token,
      timeoutMs,
    });
  } catch (error) {
    const failure = error instanceof Error ? error : new Error("iLink sendMessage transport failed");
    failure.deliveryStatus = "uncertain";
    throw failure;
  }

  let response;
  try {
    response = JSON.parse(raw);
  } catch {
    throw Object.assign(new Error("iLink sendMessage returned invalid JSON"), { deliveryStatus: "uncertain" });
  }
  if (!response || typeof response !== "object" || Array.isArray(response)) {
    throw Object.assign(new Error("iLink sendMessage returned a non-object response"), { deliveryStatus: "uncertain" });
  }
  const statusFields = ["ret", "errcode"].filter((field) => Object.hasOwn(response, field));
  if (statusFields.some((field) => !Number.isFinite(response[field]))) {
    throw Object.assign(new Error("iLink sendMessage returned an invalid status"), { deliveryStatus: "uncertain" });
  }
  const failure = statusFields.find((field) => response[field] !== 0);
  if (failure) {
    throw Object.assign(new Error(`iLink sendMessage rejected: ${failure}=${response[failure]}`), {
      deliveryStatus: "rejected",
      sessionExpired: isSessionExpired(response),
      rateLimited: typeof response.errmsg === "string" && /rate.?limit/i.test(response.errmsg),
    });
  }
  return response;
}

/**
 * 发送/取消输入状态。
 */
export async function sendTyping({ baseUrl, token, ilinkUserId, typingTicket, status = 1 }) {
  await apiPost({
    baseUrl,
    endpoint: "ilink/bot/sendtyping",
    body: JSON.stringify({
      ilink_user_id: ilinkUserId,
      typing_ticket: typingTicket,
      status,
      base_info: { bot_agent: "CodeWhale/1.0" },
    }),
    token,
    timeoutMs: 10_000,
  });
}

/**
 * 获取账号配置（含 typing_ticket）。
 */
export async function getConfig({ baseUrl, token, ilinkUserId, contextToken }) {
  const raw = await apiPost({
    baseUrl,
    endpoint: "ilink/bot/getconfig",
    body: JSON.stringify({
      ilink_user_id: ilinkUserId,
      context_token: contextToken,
      base_info: { bot_agent: "CodeWhale/1.0" },
    }),
    token,
    timeoutMs: 10_000,
  });
  return JSON.parse(raw);
}

/**
 * 通知上线。
 */
export async function notifyStart({ baseUrl, token }) {
  const raw = await apiPost({
    baseUrl,
    endpoint: "ilink/bot/msg/notifystart",
    body: JSON.stringify({ base_info: { bot_agent: "CodeWhale/1.0" } }),
    token,
  });
  return JSON.parse(raw);
}

/**
 * 通知下线。
 */
export async function notifyStop({ baseUrl, token }) {
  const raw = await apiPost({
    baseUrl,
    endpoint: "ilink/bot/msg/notifystop",
    body: JSON.stringify({ base_info: { bot_agent: "CodeWhale/1.0" } }),
    token,
  });
  return JSON.parse(raw);
}
