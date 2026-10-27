import {
  activeTurnBlock,
  cleanEnvValue,
  commandAction as coreCommandAction,
  compactRuntimeError,
  envFirst,
  isPlaceholderValue,
  latestRunningTurn,
  parseApprovalDecisionArgs,
  parseBool,
  parseCommand as coreParseCommand,
  parseEnvText,
  parseList,
  preservedChatStateFields,
  splitMessage,
  stripGroupPrefix as coreStripGroupPrefix
} from "../../bridge-core/src/lib.mjs";

export {
  activeTurnBlock,
  cleanEnvValue,
  compactRuntimeError,
  envFirst,
  isPlaceholderValue,
  latestRunningTurn,
  parseApprovalDecisionArgs,
  parseBool,
  parseEnvText,
  parseList,
  preservedChatStateFields,
  splitMessage
};

export function telegramIdentity(update) {
  const message = update?.message || update?.edited_message || {};
  const chat = message.chat || {};
  const from = message.from || {};
  const username = from.username ? `@${from.username}` : "";
  return {
    updateId: update?.update_id ?? null,
    chatId: chat.id != null ? String(chat.id) : "",
    messageId: message.message_id != null ? String(message.message_id) : "",
    chatType: chat.type || "",
    userId: from.id != null ? String(from.id) : "",
    username,
    firstName: from.first_name || "",
    text: typeof message.text === "string" ? message.text : "",
    isBot: Boolean(from.is_bot)
  };
}

export function isGroupChat(chatType) {
  return chatType === "group" || chatType === "supergroup";
}

export function isAllowed(identity, allowlist, allowUnlisted = false) {
  if (allowUnlisted) return true;
  const allowed = new Set(allowlist);
  return [identity.chatId, identity.userId, identity.username]
    .filter(Boolean)
    .some((id) => allowed.has(id));
}

export function pairingRefusalText(identity) {
  return [
    "This Telegram chat is not in TELEGRAM_CHAT_ALLOWLIST.",
    `chat_id=${identity.chatId}`,
    identity.userId ? `user_id=${identity.userId}` : "",
    identity.username ? `username=${identity.username}` : "",
    "",
    "For first pairing, add one of those IDs to TELEGRAM_CHAT_ALLOWLIST, or temporarily set TELEGRAM_ALLOW_UNLISTED=true."
  ]
    .filter(Boolean)
    .join("\n");
}

export function stripGroupPrefix(text, { chatType, requirePrefix, prefix }) {
  return coreStripGroupPrefix(text, {
    chatType,
    requirePrefix,
    prefix: prefix || "/cw",
    directChatTypes: ["private", "channel"]
  });
}

export function parseCommand(text) {
  const answer = String(text || "").trim().match(/^\/answer(?:@\w+)?(?:\s+([\s\S]*))?$/i);
  if (answer) return { name: "answer", args: (answer[1] || "").trim() };
  return coreParseCommand(text, { stripBotMention: true });
}

export function commandAction(command) {
  if (command.name === "answer") return { kind: "answer", text: command.args };
  return coreCommandAction(command, { allowMenu: true, allowStart: true });
}

export function controlKeyboard() {
  return {
    inline_keyboard: [
      [
        { text: "Status", callback_data: "cw:status" },
        { text: "New thread", callback_data: "cw:new" }
      ],
      [
        { text: "Threads", callback_data: "cw:threads" },
        { text: "Interrupt", callback_data: "cw:interrupt" }
      ],
      [
        { text: "Compact", callback_data: "cw:compact" },
        { text: "Reset model", callback_data: "cw:model:default" }
      ],
      [{ text: "Help", callback_data: "cw:help" }]
    ]
  };
}

export function activeTurnKeyboard() {
  return {
    inline_keyboard: [
      [
        { text: "Status", callback_data: "cw:status" },
        { text: "Interrupt", callback_data: "cw:interrupt" }
      ],
      [{ text: "Threads", callback_data: "cw:threads" }]
    ]
  };
}

export function approvalKeyboard(actionToken) {
  return {
    inline_keyboard: [
      [
        { text: "Allow once", callback_data: `cw:act:${actionToken}` },
        { text: "Allow + remember", callback_data: `cw:act:${actionToken}:remember` }
      ],
      [{ text: "Deny", callback_data: `cw:act:${actionToken}:deny` }]
    ]
  };
}

export function threadListKeyboard(threadActions) {
  const rows = [];
  for (const action of threadActions.slice(0, 8)) {
    rows.push([{ text: action.label, callback_data: `cw:act:${action.token}` }]);
  }
  rows.push([{ text: "New thread", callback_data: "cw:new" }]);
  return { inline_keyboard: rows };
}

export function callbackAction(data) {
  const value = String(data || "");
  switch (value) {
    case "cw:status":
      return { kind: "status" };
    case "cw:new":
      return { kind: "new_thread" };
    case "cw:threads":
      return { kind: "threads" };
    case "cw:interrupt":
      return { kind: "interrupt" };
    case "cw:compact":
      return { kind: "compact" };
    case "cw:help":
      return { kind: "help" };
    case "cw:model:default":
      return { kind: "set_model", modelName: "default" };
    default:
      break;
  }
  if (value.startsWith("cw:act:")) {
    const [, , token, suffix] = value.split(":", 4);
    return { kind: "stored_action", token: token || "", suffix: suffix || "" };
  }
  return null;
}

const MARKDOWN_V2_SPECIALS = /([_*\[\]()~`>#+\-=|{}.!\\])/g;
const BLOCK_PLACEHOLDER_PREFIX = "\u0000mdv2:block:";
const INLINE_PLACEHOLDER_PREFIX = "\u0000mdv2:inline:";
const PLACEHOLDER_SUFFIX = "\u0000";

export function telegramMessageBody(text, options = {}) {
  const maxChars = Math.floor(Number(options.maxChars) || 0);
  if (options.markdown === false) {
    return { text: boundedPlainTelegramText(text, maxChars) };
  }
  const markdownText = telegramMarkdownV2(text);
  if (maxChars > 0 && markdownText.length > maxChars) {
    return { text: boundedPlainTelegramText(text, maxChars) };
  }
  return {
    text: markdownText,
    parse_mode: "MarkdownV2"
  };
}

export function telegramMarkdownV2(text) {
  const placeholders = [];
  const source = String(text || "");
  const fenced = source.replace(/```([^\n`]*)\n?([\s\S]*?)```/g, (_match, language, body) =>
    markdownPlaceholder(
      placeholders,
      `\`\`\`${safeFenceLanguage(language)}\n${escapeMarkdownV2Code(removeClosingFenceNewline(body))}\n\`\`\``,
      BLOCK_PLACEHOLDER_PREFIX
    )
  );
  return restoreMarkdownPlaceholders(renderMarkdownLines(fenced), placeholders, BLOCK_PLACEHOLDER_PREFIX);
}

export function plainTelegramText(text) {
  const source = String(text || "");
  return renderPlainLines(
    source
      .replace(/```[^\n`]*\n?([\s\S]*?)```/g, "$1")
      .replace(/`([^`\n]+)`/g, "$1")
      .replace(/\[([^\]\n]+)\]\(([^ \n]+)\)/g, "$1 ($2)")
      .replace(/\*\*([^*\n]+)\*\*/g, "$1")
      .replace(/__([^_\n]+)__/g, "$1")
      .replace(/[*_~]/g, "")
  );
}

function boundedPlainTelegramText(text, maxChars) {
  const plain = plainTelegramText(text);
  if (maxChars > 0 && plain.length > maxChars) {
    return String(text || "");
  }
  return plain;
}

export function isTelegramMarkdownParseError(error) {
  if (Number(error?.errorCode) !== 400) return false;
  const text = String(error?.description || error?.message || "").toLowerCase();
  return (
    text.includes("parse") ||
    text.includes("can't parse entities") ||
    text.includes("entity") ||
    text.includes("markdown")
  );
}

function renderMarkdownLines(text) {
  const lines = String(text || "").split("\n");
  const output = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (isMarkdownTable(lines, index)) {
      const { rendered, nextIndex } = renderMarkdownTable(lines, index);
      output.push(rendered);
      index = nextIndex - 1;
    } else {
      output.push(renderMarkdownInline(lines[index]));
    }
  }
  return output.join("\n");
}

function renderPlainLines(text) {
  const lines = String(text || "").split("\n");
  const output = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (isMarkdownTable(lines, index)) {
      const { rendered, nextIndex } = renderPlainTable(lines, index);
      output.push(rendered);
      index = nextIndex - 1;
    } else {
      output.push(lines[index]);
    }
  }
  return output.join("\n");
}

function renderMarkdownInline(text) {
  const placeholders = [];
  let value = String(text || "");
  value = value.replace(/\[([^\]\n]+)\]\(([^ \n]+)\)/g, (_match, label, url) =>
    markdownPlaceholder(
      placeholders,
      `[${escapeMarkdownV2Text(label)}](${escapeMarkdownV2Url(url)})`,
      INLINE_PLACEHOLDER_PREFIX
    )
  );
  value = value.replace(/`([^`\n]+)`/g, (_match, code) =>
    markdownPlaceholder(placeholders, `\`${escapeMarkdownV2Code(code)}\``, INLINE_PLACEHOLDER_PREFIX)
  );
  value = value.replace(/\*\*([^*\n]+)\*\*/g, (_match, body) =>
    markdownPlaceholder(placeholders, `*${escapeMarkdownV2Text(body)}*`, INLINE_PLACEHOLDER_PREFIX)
  );
  value = escapeMarkdownV2Text(value);
  return restoreMarkdownPlaceholders(value, placeholders, INLINE_PLACEHOLDER_PREFIX);
}

function renderMarkdownTable(lines, startIndex) {
  const headers = tableCells(lines[startIndex]);
  const rows = [];
  let index = startIndex + 2;
  while (index < lines.length && looksLikeTableRow(lines[index])) {
    rows.push(tableCells(lines[index]));
    index += 1;
  }
  const headerText = headers.map(escapeMarkdownV2Text).join(" / ");
  const rendered = [`*${headerText}*`];
  for (const row of rows) {
    const fields = headers.map((header, cellIndex) => {
      const value = row[cellIndex] || "";
      return `${escapeMarkdownV2Text(header)}: ${escapeMarkdownV2Text(value)}`;
    });
    rendered.push(`• ${fields.join("; ")}`);
  }
  return { rendered: rendered.join("\n"), nextIndex: index };
}

function renderPlainTable(lines, startIndex) {
  const headers = tableCells(lines[startIndex]);
  const rows = [];
  let index = startIndex + 2;
  while (index < lines.length && looksLikeTableRow(lines[index])) {
    rows.push(tableCells(lines[index]));
    index += 1;
  }
  const rendered = [headers.join(" / ")];
  for (const row of rows) {
    const fields = headers.map((header, cellIndex) => `${header}: ${row[cellIndex] || ""}`);
    rendered.push(`- ${fields.join("; ")}`);
  }
  return { rendered: rendered.join("\n"), nextIndex: index };
}

function isMarkdownTable(lines, index) {
  return (
    looksLikeTableRow(lines[index]) &&
    index + 1 < lines.length &&
    looksLikeTableSeparator(lines[index + 1])
  );
}

function looksLikeTableRow(line) {
  return tableCells(line).length >= 2;
}

function looksLikeTableSeparator(line) {
  const cells = tableCells(line);
  return cells.length >= 2 && cells.every((cell) => /^:?-{3,}:?$/.test(cell));
}

function tableCells(line) {
  const trimmed = String(line || "").trim();
  if (!trimmed.includes("|")) return [];
  return trimmed
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((cell) => cell.trim());
}

function escapeMarkdownV2Text(text) {
  return String(text || "").replace(MARKDOWN_V2_SPECIALS, "\\$1");
}

function escapeMarkdownV2Code(text) {
  return String(text || "").replace(/([`\\])/g, "\\$1");
}

function escapeMarkdownV2Url(text) {
  return String(text || "").replace(/([)\\])/g, "\\$1");
}

function safeFenceLanguage(language) {
  return String(language || "").trim().replace(/[^\w+-]/g, "");
}

function removeClosingFenceNewline(text) {
  return String(text || "").replace(/\n$/, "");
}

function markdownPlaceholder(placeholders, rendered, prefix) {
  const index = placeholders.push(rendered) - 1;
  return `${prefix}${index}${PLACEHOLDER_SUFFIX}`;
}

function restoreMarkdownPlaceholders(text, placeholders, prefix) {
  return String(text || "").replace(
    new RegExp(`${prefix}(\\d+)${PLACEHOLDER_SUFFIX}`, "g"),
    (_match, index) => placeholders[Number(index)] || ""
  );
}

export function telegramRetryDelayMs(error, fallbackMs = 3000) {
  const retryAfter = Number(error?.parameters?.retry_after || 0);
  if (Number.isFinite(retryAfter) && retryAfter > 0) {
    return Math.min(retryAfter * 1000, 60000);
  }
  return fallbackMs;
}

const POLLING_CONFLICT_DELAYS_MS = [15000, 25000, 35000, 45000, 55000];

export function telegramPollingConflictDelayMs(attempt = 0) {
  const index = Math.max(0, Math.floor(Number(attempt) || 0));
  return POLLING_CONFLICT_DELAYS_MS[index] ?? null;
}

export function telegramSendRetryDelayMs(error, attempt = 0) {
  const retryAfter = Number(error?.parameters?.retry_after || 0);
  if (error?.errorCode === 429 && attempt < 3) {
    if (Number.isFinite(retryAfter) && retryAfter > 0) {
      return Math.min(retryAfter * 1000, 60000);
    }
    return 3000;
  }
  return null;
}

export function looksLikePollingConflict(error) {
  const text = String(error?.description || error?.message || "").toLowerCase();
  return error?.errorCode === 409 || text.includes("terminated by other getupdates request");
}

export function validateBridgeConfig(env, options = {}) {
  const runtimeEnv = options.runtimeEnv || null;
  const workspaceRoot = options.workspaceRoot || "";
  const errors = [];
  const warnings = [];
  const info = [];
  const add = (list, code, message) => list.push({ code, message });

  const botToken = envFirst(env, "TELEGRAM_BOT_TOKEN");
  if (!botToken) {
    add(errors, "missing_required", "TELEGRAM_BOT_TOKEN is required");
  } else if (isPlaceholderValue(botToken)) {
    add(errors, "placeholder_value", "TELEGRAM_BOT_TOKEN still contains a placeholder value");
  }

  const runtimeUrl = envFirst(env, "CODEWHALE_RUNTIME_URL", "DEEPSEEK_RUNTIME_URL") || "http://127.0.0.1:7878";
  try {
    const parsed = new URL(runtimeUrl);
    const localHosts = new Set(["127.0.0.1", "localhost", "[::1]", "::1"]);
    if (!["http:", "https:"].includes(parsed.protocol)) {
      add(errors, "invalid_runtime_url", "CODEWHALE_RUNTIME_URL must use http or https");
    }
    if (!localHosts.has(parsed.hostname) && options.requireLocalRuntime !== false) {
      add(errors, "remote_runtime_url", "CODEWHALE_RUNTIME_URL should point at localhost on a VM deployment");
    }
  } catch {
    add(errors, "invalid_runtime_url", "CODEWHALE_RUNTIME_URL is not a valid URL");
  }

  const runtimeToken = envFirst(env, "CODEWHALE_RUNTIME_TOKEN", "DEEPSEEK_RUNTIME_TOKEN");
  if (!runtimeToken) {
    add(errors, "missing_required", "CODEWHALE_RUNTIME_TOKEN is required");
  } else if (isPlaceholderValue(runtimeToken)) {
    add(errors, "placeholder_value", "CODEWHALE_RUNTIME_TOKEN still contains a placeholder value");
  }

  const workspace = envFirst(env, "CODEWHALE_WORKSPACE", "DEEPSEEK_WORKSPACE");
  if (workspace && !workspace.startsWith("/")) {
    add(errors, "relative_workspace", "CODEWHALE_WORKSPACE must be an absolute path");
  }
  if (
    workspace &&
    workspaceRoot &&
    workspace !== workspaceRoot &&
    !workspace.startsWith(`${workspaceRoot}/`)
  ) {
    add(warnings, "workspace_root", `CODEWHALE_WORKSPACE is outside ${workspaceRoot}`);
  }

  const threadMapPath = envFirst(env, "TELEGRAM_THREAD_MAP_PATH");
  if (threadMapPath && !threadMapPath.startsWith("/")) {
    add(errors, "relative_thread_map", "TELEGRAM_THREAD_MAP_PATH must be an absolute path");
  }

  const allowGroups = parseBool(env.TELEGRAM_ALLOW_GROUPS, false);
  const requirePrefix = parseBool(env.TELEGRAM_REQUIRE_PREFIX_IN_GROUP, true);
  const allowUnlisted = parseBool(
    envFirst(env, "TELEGRAM_ALLOW_UNLISTED", "CODEWHALE_ALLOW_UNLISTED", "DEEPSEEK_ALLOW_UNLISTED"),
    false
  );
  const allowlist = parseList(
    envFirst(env, "TELEGRAM_CHAT_ALLOWLIST", "CODEWHALE_CHAT_ALLOWLIST", "DEEPSEEK_CHAT_ALLOWLIST")
  );

  if (!allowlist.length && allowUnlisted) {
    add(warnings, "pairing_mode_open", "TELEGRAM_ALLOW_UNLISTED=true leaves first-pairing mode open");
  } else if (!allowlist.length) {
    add(warnings, "not_paired", "TELEGRAM_CHAT_ALLOWLIST is empty; all chats will be refused");
  }
  if (allowGroups && allowUnlisted) {
    add(errors, "open_group_control", "Group control cannot be enabled while unlisted chats are allowed");
  }
  if (allowGroups && !requirePrefix) {
    add(warnings, "group_without_prefix", "Group control is enabled without requiring TELEGRAM_GROUP_PREFIX");
  }
  if (!allowGroups) {
    add(info, "dm_only", "Direct-message control is enabled; group chats are disabled");
  }

  const maxReplyChars = Number(env.TELEGRAM_MAX_REPLY_CHARS || 3500);
  if (!Number.isFinite(maxReplyChars) || maxReplyChars < 100 || maxReplyChars > 4096) {
    add(errors, "invalid_max_reply_chars", "TELEGRAM_MAX_REPLY_CHARS must be between 100 and 4096");
  }
  const pollTimeout = Number(env.TELEGRAM_POLL_TIMEOUT_SECONDS || 50);
  if (!Number.isFinite(pollTimeout) || pollTimeout < 1 || pollTimeout > 60) {
    add(errors, "invalid_poll_timeout", "TELEGRAM_POLL_TIMEOUT_SECONDS must be between 1 and 60");
  }
  const turnTimeoutMs = Number(envFirst(env, "CODEWHALE_TURN_TIMEOUT_MS", "DEEPSEEK_TURN_TIMEOUT_MS") || 900000);
  if (!Number.isFinite(turnTimeoutMs) || turnTimeoutMs < 1000) {
    add(errors, "invalid_turn_timeout", "CODEWHALE_TURN_TIMEOUT_MS must be at least 1000");
  }

  if (runtimeEnv) {
    const runtimeFileToken = envFirst(runtimeEnv, "CODEWHALE_RUNTIME_TOKEN", "DEEPSEEK_RUNTIME_TOKEN");
    if (!runtimeFileToken) {
      add(errors, "missing_runtime_token", "runtime.env is missing CODEWHALE_RUNTIME_TOKEN");
    } else if (isPlaceholderValue(runtimeFileToken)) {
      add(errors, "placeholder_runtime_token", "runtime.env CODEWHALE_RUNTIME_TOKEN is still a placeholder");
    } else if (runtimeToken && runtimeToken !== runtimeFileToken) {
      add(errors, "token_mismatch", "Runtime and bridge token values do not match");
    }

    const provider = envFirst(runtimeEnv, "CODEWHALE_PROVIDER", "DEEPSEEK_PROVIDER");
    if (!provider) {
      add(warnings, "missing_provider", "runtime.env does not set CODEWHALE_PROVIDER");
    }

    const runtimePort = Number(envFirst(runtimeEnv, "CODEWHALE_RUNTIME_PORT", "DEEPSEEK_RUNTIME_PORT") || 7878);
    if (!Number.isInteger(runtimePort) || runtimePort <= 0 || runtimePort > 65535) {
      add(errors, "invalid_runtime_port", "runtime port must be a valid TCP port");
    }
  }

  return {
    ok: errors.length === 0,
    errors,
    warnings,
    info
  };
}

export function formatValidationReport(result) {
  const lines = ["Telegram bridge config validation"];
  for (const item of result.errors) lines.push(`[fail] ${item.message}`);
  for (const item of result.warnings) lines.push(`[warn] ${item.message}`);
  for (const item of result.info) lines.push(`[info] ${item.message}`);
  if (result.ok) lines.push("[ok] No blocking config errors found");
  return lines.join("\n");
}

export function helpText() {
  return [
    "CodeWhale Telegram bridge commands:",
    "/menu - open tappable controls",
    "/help - show this help",
    "/status - runtime and workspace status",
    "/threads - recent runtime threads",
    "/new - create a new thread for this chat",
    "/resume <thread_id> - bind this chat to an existing thread",
    "/model <name|default> - set or reset this chat's model",
    "/interrupt - interrupt the active turn",
    "/compact - compact the current thread",
    "/allow <approval_id> [remember] - approve a pending tool call",
    "/deny <approval_id> - deny a pending tool call",
    "/answer <response> - answer the current agent question",
    "",
    "Anything else is sent as a CodeWhale prompt."
  ].join("\n");
}
