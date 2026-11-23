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
  parseEnvText,
  parseList,
  parseTextContent as coreParseTextContent,
  preservedChatStateFields as corePreservedChatStateFields,
  splitMessage,
  stripGroupPrefix as coreStripGroupPrefix
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
  parseEnvText,
  parseList,
  splitMessage
};

export function parseTextContent(content) {
  return coreParseTextContent(content, ["text", "content"]);
}

export function incomingIdentity(event) {
  const sender = event?.sender?.sender_id || {};
  const message = event?.message || {};
  return {
    chatId: message.chat_id || "",
    messageId: message.message_id || "",
    chatType: message.chat_type || "",
    messageType: message.message_type || "",
    openId: sender.open_id || "",
    unionId: sender.union_id || "",
    userId: sender.user_id || "",
    // Thread/topic group context: these fields let the bridge reply
    // inside the same topic instead of spawning a new standalone topic.
    // / 话题群上下文：用于在同一话题内回复，而非新建独立话题。
    parentId: message.parent_id || "",
    rootId: message.root_id || "",
    threadId: message.thread_id || ""
  };
}

export function isAllowed(identity, allowlist, allowUnlisted = false) {
  if (allowUnlisted) return true;
  const allowed = new Set(allowlist);
  return [identity.chatId, identity.openId, identity.unionId, identity.userId]
    .filter(Boolean)
    .some((id) => allowed.has(id));
}

export function pairingRefusalText(identity) {
  return [
    "This chat is not in DEEPSEEK_CHAT_ALLOWLIST.",
    `chat_id=${identity.chatId}`,
    identity.openId ? `open_id=${identity.openId}` : "",
    identity.unionId ? `union_id=${identity.unionId}` : "",
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
    directChatTypes: ["p2p"]
  });
}

export function commandAction(command) {
  return coreCommandAction(command);
}

export function preservedChatStateFields(state = {}) {
  return corePreservedChatStateFields(state, ["model", "replyToMessageId", "authorizedIdentity"]);
}

export function validateBridgeConfig(env, options = {}) {
  const runtimeEnv = options.runtimeEnv || null;
  const workspaceRoot = options.workspaceRoot || "";
  const errors = [];
  const warnings = [];
  const info = [];
  const add = (list, code, message) => list.push({ code, message });

  for (const key of [
    "FEISHU_APP_ID",
    "FEISHU_APP_SECRET",
    "DEEPSEEK_RUNTIME_URL",
    "DEEPSEEK_RUNTIME_TOKEN",
    "DEEPSEEK_WORKSPACE",
    "FEISHU_THREAD_MAP_PATH"
  ]) {
    const value = cleanEnvValue(env[key]);
    if (!value) {
      add(errors, "missing_required", `${key} is required`);
    } else if (isPlaceholderValue(value)) {
      add(errors, "placeholder_value", `${key} still contains a placeholder value`);
    }
  }

  const domain = cleanEnvValue(env.FEISHU_DOMAIN || "feishu").toLowerCase();
  if (!["feishu", "lark"].includes(domain) && !/^https:\/\/open\./.test(domain)) {
    add(errors, "invalid_domain", "FEISHU_DOMAIN must be feishu, lark, or an https://open.* URL");
  }

  const runtimeUrl = cleanEnvValue(env.DEEPSEEK_RUNTIME_URL || "http://127.0.0.1:7878");
  try {
    const parsed = new URL(runtimeUrl);
    const localHosts = new Set(["127.0.0.1", "localhost", "[::1]", "::1"]);
    if (!["http:", "https:"].includes(parsed.protocol)) {
      add(errors, "invalid_runtime_url", "DEEPSEEK_RUNTIME_URL must use http or https");
    }
    if (!localHosts.has(parsed.hostname)) {
      add(errors, "remote_runtime_url", "DEEPSEEK_RUNTIME_URL must point at localhost on Lighthouse");
    }
  } catch {
    add(errors, "invalid_runtime_url", "DEEPSEEK_RUNTIME_URL is not a valid URL");
  }

  const workspace = cleanEnvValue(env.DEEPSEEK_WORKSPACE);
  if (workspace && !workspace.startsWith("/")) {
    add(errors, "relative_workspace", "DEEPSEEK_WORKSPACE must be an absolute path");
  }
  if (
    workspace &&
    workspaceRoot &&
    workspace !== workspaceRoot &&
    !workspace.startsWith(`${workspaceRoot}/`)
  ) {
    add(warnings, "workspace_root", `DEEPSEEK_WORKSPACE is outside ${workspaceRoot}`);
  }

  const threadMapPath = cleanEnvValue(env.FEISHU_THREAD_MAP_PATH);
  if (threadMapPath && !threadMapPath.startsWith("/")) {
    add(errors, "relative_thread_map", "FEISHU_THREAD_MAP_PATH must be an absolute path");
  }

  const allowGroups = parseBool(env.FEISHU_ALLOW_GROUPS, false);
  const requirePrefix = parseBool(env.FEISHU_REQUIRE_PREFIX_IN_GROUP, true);
  const allowUnlisted = parseBool(env.DEEPSEEK_ALLOW_UNLISTED, false);
  const allowlist = parseList(env.DEEPSEEK_CHAT_ALLOWLIST);

  if (!allowlist.length && allowUnlisted) {
    add(warnings, "pairing_mode_open", "DEEPSEEK_ALLOW_UNLISTED=true leaves first-pairing mode open");
  } else if (!allowlist.length) {
    add(warnings, "not_paired", "DEEPSEEK_CHAT_ALLOWLIST is empty; all chats will be refused");
  }
  if (allowGroups && allowUnlisted) {
    add(errors, "open_group_control", "Group control cannot be enabled while unlisted chats are allowed");
  }
  if (allowGroups && !requirePrefix) {
    add(warnings, "group_without_prefix", "Group control is enabled without requiring FEISHU_GROUP_PREFIX");
  }
  if (!allowGroups) {
    add(info, "dm_only", "Direct-message control is enabled; group chats are disabled");
  }

  const maxReplyChars = Number(env.FEISHU_MAX_REPLY_CHARS || 3500);
  if (!Number.isFinite(maxReplyChars) || maxReplyChars < 100 || maxReplyChars > FEISHU_MAX_TEXT_CHARS) {
    add(errors, "invalid_max_reply_chars", `FEISHU_MAX_REPLY_CHARS must be between 100 and ${FEISHU_MAX_TEXT_CHARS}`);
  }
  const turnTimeoutMs = Number(env.DEEPSEEK_TURN_TIMEOUT_MS || 900000);
  if (!Number.isFinite(turnTimeoutMs) || turnTimeoutMs < 1000) {
    add(errors, "invalid_turn_timeout", "DEEPSEEK_TURN_TIMEOUT_MS must be at least 1000");
  }

  if (runtimeEnv) {
    const runtimeToken = cleanEnvValue(runtimeEnv.DEEPSEEK_RUNTIME_TOKEN);
    const bridgeToken = cleanEnvValue(env.DEEPSEEK_RUNTIME_TOKEN);
    if (!runtimeToken) {
      add(errors, "missing_runtime_token", "runtime.env is missing DEEPSEEK_RUNTIME_TOKEN");
    } else if (isPlaceholderValue(runtimeToken)) {
      add(errors, "placeholder_runtime_token", "runtime.env DEEPSEEK_RUNTIME_TOKEN is still a placeholder");
    } else if (bridgeToken && bridgeToken !== runtimeToken) {
      add(errors, "token_mismatch", "Runtime and bridge DEEPSEEK_RUNTIME_TOKEN values do not match");
    }

    const apiKey = cleanEnvValue(runtimeEnv.DEEPSEEK_API_KEY);
    if (!apiKey) {
      add(warnings, "missing_api_key", "runtime.env is missing DEEPSEEK_API_KEY");
    } else if (isPlaceholderValue(apiKey)) {
      add(warnings, "placeholder_api_key", "runtime.env DEEPSEEK_API_KEY is still a placeholder");
    }

    const runtimePort = Number(runtimeEnv.DEEPSEEK_RUNTIME_PORT || 7878);
    if (!Number.isInteger(runtimePort) || runtimePort <= 0 || runtimePort > 65535) {
      add(errors, "invalid_runtime_port", "DEEPSEEK_RUNTIME_PORT must be a valid TCP port");
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
  const lines = ["Feishu bridge config validation"];
  for (const item of result.errors) lines.push(`[fail] ${item.message}`);
  for (const item of result.warnings) lines.push(`[warn] ${item.message}`);
  for (const item of result.info) lines.push(`[info] ${item.message}`);
  if (result.ok) lines.push("[ok] No blocking config errors found");
  return lines.join("\n");
}

export const FEISHU_MAX_TEXT_CHARS = 30000;

export function helpText() {
  return [
    "CodeWhale phone bridge commands:",
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
    "",
    "Anything else is sent as a CodeWhale prompt."
  ].join("\n");
}
