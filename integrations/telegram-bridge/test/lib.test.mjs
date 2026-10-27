import test from "node:test";
import assert from "node:assert/strict";

import {
  activeTurnBlock,
  activeTurnKeyboard,
  approvalKeyboard,
  callbackAction,
  commandAction,
  controlKeyboard,
  envFirst,
  helpText,
  isAllowed,
  pairingRefusalText,
  parseApprovalDecisionArgs,
  parseBool,
  parseCommand,
  parseEnvText,
  parseList,
  preservedChatStateFields,
  splitMessage,
  stripGroupPrefix,
  threadListKeyboard,
  telegramIdentity,
  telegramMarkdownV2,
  telegramMessageBody,
  plainTelegramText,
  telegramPollingConflictDelayMs,
  telegramRetryDelayMs,
  telegramSendRetryDelayMs,
  isTelegramMarkdownParseError,
  looksLikePollingConflict,
  validateBridgeConfig
} from "../src/lib.mjs";

test("envFirst returns first non-empty value", () => {
  assert.equal(envFirst({ A: "", B: " value " }, "A", "B"), "value");
  assert.equal(envFirst({ A: "x" }, "B"), "");
});

test("parseList trims empty values", () => {
  assert.deepEqual(parseList(" 123, @user ,, "), ["123", "@user"]);
});

test("parseBool accepts common truthy values", () => {
  assert.equal(parseBool("yes"), true);
  assert.equal(parseBool("0", true), false);
  assert.equal(parseBool(undefined, true), true);
});

test("parseEnvText handles comments, export, and quoted values", () => {
  assert.deepEqual(
    parseEnvText(`
      # ignored
      export TELEGRAM_GROUP_PREFIX="/cw"
      CODEWHALE_WORKSPACE='/opt/whalebro'
    `),
    {
      TELEGRAM_GROUP_PREFIX: "/cw",
      CODEWHALE_WORKSPACE: "/opt/whalebro"
    }
  );
});

test("telegramIdentity extracts chat and sender identifiers", () => {
  const identity = telegramIdentity({
    update_id: 10,
    message: {
      message_id: 20,
      text: "hello",
      chat: { id: -1001, type: "supergroup" },
      from: { id: 42, username: "hunter", first_name: "Hunter" }
    }
  });
  assert.deepEqual(identity, {
    updateId: 10,
    chatId: "-1001",
    messageId: "20",
    chatType: "supergroup",
    userId: "42",
    username: "@hunter",
    firstName: "Hunter",
    text: "hello",
    isBot: false
  });
});

test("stripGroupPrefix requires prefix in Telegram groups", () => {
  assert.deepEqual(
    stripGroupPrefix("/cw inspect this", {
      chatType: "group",
      requirePrefix: true,
      prefix: "/cw"
    }),
    { accepted: true, text: "inspect this" }
  );
  assert.equal(
    stripGroupPrefix("inspect this", {
      chatType: "group",
      requirePrefix: true,
      prefix: "/cw"
    }).accepted,
    false
  );
});

test("stripGroupPrefix accepts private chat text without group prefix", () => {
  assert.deepEqual(
    stripGroupPrefix("inspect this", {
      chatType: "private",
      requirePrefix: true,
      prefix: "/cw"
    }),
    { accepted: true, text: "inspect this" }
  );
});

test("stripGroupPrefix accepts Telegram channel text without group prefix", () => {
  assert.deepEqual(
    stripGroupPrefix("inspect this", {
      chatType: "channel",
      requirePrefix: true,
      prefix: "/cw"
    }),
    { accepted: true, text: "inspect this" }
  );
});

test("parseCommand handles Telegram bot mentions", () => {
  assert.deepEqual(parseCommand("hello"), { name: "prompt", args: "hello" });
  assert.deepEqual(parseCommand("/allow@CodeWhaleBot abc remember"), {
    name: "allow",
    args: "abc remember"
  });
});

test("commandAction maps bridge commands and falls back to prompts", () => {
  assert.deepEqual(commandAction(parseCommand("/menu")), { kind: "menu" });
  assert.deepEqual(commandAction(parseCommand("/status")), { kind: "status" });
  assert.deepEqual(commandAction(parseCommand("/resume thread-1")), {
    kind: "resume",
    threadId: "thread-1"
  });
  assert.deepEqual(commandAction(parseCommand("/model arcee-trinity")), {
    kind: "set_model",
    modelName: "arcee-trinity"
  });
  assert.deepEqual(commandAction(parseCommand("/unknown value")), {
    kind: "prompt",
    prompt: "/unknown value"
  });
});

test("helpText documents per-chat model switching", () => {
  assert.match(helpText(), /\/model <name\|default>/);
  assert.match(helpText(), /\/menu/);
});

test("control keyboards expose modal actions", () => {
  assert.deepEqual(controlKeyboard().inline_keyboard[0][0], {
    text: "Status",
    callback_data: "cw:status"
  });
  assert.deepEqual(activeTurnKeyboard().inline_keyboard[0][1], {
    text: "Interrupt",
    callback_data: "cw:interrupt"
  });
  assert.deepEqual(approvalKeyboard("tok1").inline_keyboard[1][0], {
    text: "Deny",
    callback_data: "cw:act:tok1:deny"
  });
  assert.deepEqual(threadListKeyboard([{ token: "t1", label: "Resume 1" }]).inline_keyboard[0][0], {
    text: "Resume 1",
    callback_data: "cw:act:t1"
  });
});

test("callbackAction parses modal callback payloads", () => {
  assert.deepEqual(callbackAction("cw:status"), { kind: "status" });
  assert.deepEqual(callbackAction("cw:model:default"), {
    kind: "set_model",
    modelName: "default"
  });
  assert.deepEqual(callbackAction("cw:act:tok1:remember"), {
    kind: "stored_action",
    token: "tok1",
    suffix: "remember"
  });
  assert.equal(callbackAction("unknown"), null);
});

test("preservedChatStateFields carries model across state replacement", () => {
  assert.deepEqual(
    preservedChatStateFields({
      threadId: "old-thread",
      model: "mimo-v2.5-pro",
      activeTurnId: "turn-1"
    }),
    {
      model: "mimo-v2.5-pro"
    }
  );
  assert.deepEqual(preservedChatStateFields({ model: null }), { model: null });
});

test("parseApprovalDecisionArgs extracts remember flag", () => {
  assert.deepEqual(parseApprovalDecisionArgs("ap_123 remember"), {
    approvalId: "ap_123",
    remember: true
  });
  assert.deepEqual(parseApprovalDecisionArgs(""), { approvalId: "", remember: false });
});

test("isAllowed checks Telegram chat/user/username identifiers", () => {
  assert.equal(
    isAllowed({ chatId: "-1001", userId: "42", username: "@hunter" }, ["42"], false),
    true
  );
  assert.equal(isAllowed({ chatId: "-1001" }, [], false), false);
  assert.equal(isAllowed({ chatId: "-1001" }, [], true), true);
});

test("pairingRefusalText includes allowlist identifiers", () => {
  const body = pairingRefusalText({
    chatId: "-1001",
    userId: "42",
    username: "@hunter"
  });
  assert.match(body, /chat_id=-1001/);
  assert.match(body, /user_id=42/);
  assert.match(body, /username=@hunter/);
});

test("activeTurnBlock reports active queued or in-progress turn", () => {
  assert.equal(activeTurnBlock({ turns: [{ id: "done", status: "completed" }] }), null);
  assert.deepEqual(
    activeTurnBlock({
      turns: [
        { id: "old", status: "completed" },
        { id: "turn-2", status: "queued" }
      ]
    }),
    {
      turnId: "turn-2",
      message: "Thread already has active turn turn-2. Wait for it to finish or send /interrupt."
    }
  );
});

test("splitMessage chunks long text without splitting surrogate pairs", () => {
  assert.deepEqual(splitMessage("a🧪b", 2), ["a🧪", "b"]);
});

test("telegramMarkdownV2 escapes text while preserving useful markdown", () => {
  assert.equal(
    telegramMarkdownV2("**Build** passed for [CI](https://example.com/a_(b))."),
    "*Build* passed for [CI](https://example.com/a_(b\\))\\."
  );
  assert.equal(telegramMarkdownV2("Use `cargo test -p codewhale`."), "Use `cargo test -p codewhale`\\.");
  assert.equal(telegramMarkdownV2("Path C:\\tmp\\file"), "Path C:\\\\tmp\\\\file");
  assert.equal(
    telegramMarkdownV2("```rust\nfn main() { println!(\"hi\"); }\n```"),
    "```rust\nfn main() { println!(\"hi\"); }\n```"
  );
});

test("telegramMarkdownV2 rewrites pipe tables into phone-readable bullets", () => {
  assert.equal(
    telegramMarkdownV2("| Gate | Result |\n| --- | --- |\n| Lint | Pass |\n| Tests | Fail |"),
    "*Gate / Result*\n• Gate: Lint; Result: Pass\n• Gate: Tests; Result: Fail"
  );
});

test("telegram message bodies can fall back from MarkdownV2 to plain text", () => {
  assert.deepEqual(telegramMessageBody("**Done**", { markdown: true }), {
    text: "*Done*",
    parse_mode: "MarkdownV2"
  });
  assert.deepEqual(telegramMessageBody("!!!!", { markdown: true, maxChars: 4 }), {
    text: "!!!!"
  });
  assert.deepEqual(telegramMessageBody("**Done**", { markdown: false }), {
    text: "Done"
  });
  assert.equal(plainTelegramText("[CI](https://example.com) **passed**"), "CI (https://example.com) passed");
  assert.equal(
    isTelegramMarkdownParseError({ errorCode: 400, description: "Bad Request: can't parse entities" }),
    true
  );
});

test("telegramRetryDelayMs honors retry_after", () => {
  assert.equal(telegramRetryDelayMs({ parameters: { retry_after: 2 } }), 2000);
});

test("telegramPollingConflictDelayMs escalates before going fatal", () => {
  assert.deepEqual(
    [0, 1, 2, 3, 4, 5].map((attempt) => telegramPollingConflictDelayMs(attempt)),
    [15000, 25000, 35000, 45000, 55000, null]
  );
});

test("telegramSendRetryDelayMs retries only safe send failures", () => {
  assert.equal(
    telegramSendRetryDelayMs({ errorCode: 429, parameters: { retry_after: 3 } }, 0),
    3000
  );
  assert.equal(
    telegramSendRetryDelayMs({ errorCode: 429, parameters: { retry_after: 3 } }, 3),
    null
  );
  assert.equal(telegramSendRetryDelayMs(new TypeError("fetch failed"), 0), null);
  assert.equal(telegramSendRetryDelayMs(new TypeError("fetch failed"), 1), null);
  assert.equal(telegramSendRetryDelayMs(new TypeError("fetch failed"), 2), null);
  assert.equal(telegramSendRetryDelayMs({ name: "AbortError" }, 0), null);
  assert.equal(telegramSendRetryDelayMs({ errorCode: 500 }, 0), null);
});

test("looksLikePollingConflict detects Telegram 409 conflicts", () => {
  assert.equal(looksLikePollingConflict({ errorCode: 409 }), true);
  assert.equal(
    looksLikePollingConflict({
      message: "Conflict: terminated by other getUpdates request"
    }),
    true
  );
});

test("validateBridgeConfig accepts locked-down whalebro DM config", () => {
  const result = validateBridgeConfig(
    {
      TELEGRAM_BOT_TOKEN: "123456:token",
      CODEWHALE_RUNTIME_URL: "http://127.0.0.1:7878",
      CODEWHALE_RUNTIME_TOKEN: "token-a",
      CODEWHALE_WORKSPACE: "/opt/whalebro",
      TELEGRAM_CHAT_ALLOWLIST: "42",
      TELEGRAM_ALLOW_UNLISTED: "false",
      TELEGRAM_THREAD_MAP_PATH: "/var/lib/codewhale-telegram-bridge/thread-map.json",
      TELEGRAM_ALLOW_GROUPS: "false",
      TELEGRAM_REQUIRE_PREFIX_IN_GROUP: "true"
    },
    {
      workspaceRoot: "/opt/whalebro",
      runtimeEnv: {
        CODEWHALE_RUNTIME_TOKEN: "token-a",
        CODEWHALE_PROVIDER: "arcee",
        CODEWHALE_RUNTIME_PORT: "7878"
      }
    }
  );
  assert.equal(result.ok, true);
  assert.equal(result.errors.length, 0);
});

test("validateBridgeConfig rejects unsafe group pairing and token mismatch", () => {
  const result = validateBridgeConfig(
    {
      TELEGRAM_BOT_TOKEN: "123456:token",
      CODEWHALE_RUNTIME_URL: "http://127.0.0.1:7878",
      CODEWHALE_RUNTIME_TOKEN: "bridge-token",
      CODEWHALE_WORKSPACE: "/opt/whalebro",
      TELEGRAM_ALLOW_UNLISTED: "true",
      TELEGRAM_THREAD_MAP_PATH: "/var/lib/codewhale-telegram-bridge/thread-map.json",
      TELEGRAM_ALLOW_GROUPS: "true",
      TELEGRAM_REQUIRE_PREFIX_IN_GROUP: "false"
    },
    {
      workspaceRoot: "/opt/whalebro",
      runtimeEnv: {
        CODEWHALE_RUNTIME_TOKEN: "runtime-token",
        CODEWHALE_PROVIDER: "arcee"
      }
    }
  );
  assert.equal(result.ok, false);
  assert.match(
    result.errors.map((item) => item.code).join(","),
    /open_group_control/
  );
  assert.match(result.errors.map((item) => item.code).join(","), /token_mismatch/);
  assert.match(result.warnings.map((item) => item.code).join(","), /group_without_prefix/);
});
