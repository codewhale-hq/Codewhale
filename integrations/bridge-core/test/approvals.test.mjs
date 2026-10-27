import test from "node:test";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { once } from "node:events";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import url from "node:url";
import {
  ApprovalOwnershipError, ThreadStore, decideApproval, settleStoredApproval,
  admitTurn, createRuntimeClient, activeTurnBlock, latestRunningTurn, readSse, readJsonSafe, compactRuntimeError
} from "../src/lib.mjs";
import { isAllowed, isGroupChat } from "../../telegram-bridge/src/lib.mjs";
import { incomingIdentity as wecomIdentity } from "../../wecom-bridge/src/lib.mjs";

const root = path.resolve(path.dirname(url.fileURLToPath(import.meta.url)), "..", "..");
const chatId = "chat-a", threadId = "thread-a", turnId = "turn-a";
const owner = { chatId, threadId, turnId, actorId: "telegram-user:alice" };
const identities = {
  "telegram-bridge": (userId) => ({ chatId, chatType: "group", userId, isBot: false }),
  "feishu-bridge": (openId) => ({ chatId, chatType: "group", openId }),
  "wecom-bridge": (userId) => ({ body: { chatid: chatId, chattype: "group", from: { userid: userId } } }),
  "weixin-bridge": () => undefined
};
const actors = {
  "telegram-bridge": "telegram-user:alice", "feishu-bridge": "feishu-openId:alice",
  "wecom-bridge": "wecom-user:alice", "weixin-bridge": "weixin-account:bot-a:user:chat-a"
};
async function withStore(fn, options = {}) {
  const dir = await mkdtemp(path.join(tmpdir(), "bridge-approvals-"));
  try {
    const store = await ThreadStore.open(path.join(dir, "state.json"), { actions: true, messageLimit: 500, ...options });
    await store.setChat(chatId, { threadId });
    await fn(store);
  } finally { await rm(dir, { recursive: true, force: true }); }
}
function extract(source, name) {
  const marker = new RegExp(`^(?:async )?function ${name}\\(`, "m");
  const start = source.search(marker);
  assert.notEqual(start, -1, name);
  const rest = source.slice(start);
  const next = rest.search(/\n(?:async )?function \w+\(/);
  return next === -1 ? rest : rest.slice(0, next);
}
async function shipping(bridge, names, values) {
  const source = await readFile(path.join(root, bridge, "src/index.mjs"), "utf8");
  const functions = [...new Set([
    ...(bridge === "weixin-bridge" && names.includes("turnActor") ? ["accountIdentity"] : []),
    ...(bridge === "telegram-bridge" && (names.includes("runPrompt") || names.includes("startPromptTurn"))
      ? ["recoveryAllowed", "supportsAdmissionRecovery"] : []), ...names
  ])];
  return new Function(...Object.keys(values), functions.map((name) => extract(source, name)).join("\n") +
    `\nreturn { ${names.join(", ")} };`)(...Object.values(values));
}
async function withRuntime(fn) {
  const state = { pending: [{ id: "approval-a", turn_id: turnId }], turns: [], posts: [],
    startAttempts: 0, accepted: [], operations: new Map(), failStart: false, acceptDecision: true, events: [] };
  const server = createServer(async (request, response) => {
    assert.equal(request.headers.authorization, "Bearer bridge-runtime-fixture-token");
    let text = "";
    for await (const chunk of request) text += chunk;
    response.setHeader("connection", "close");
    if (request.url.includes("/events?")) {
      response.setHeader("content-type", "text/event-stream");
      response.end(state.events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join(""));
      return;
    }
    response.setHeader("content-type", "application/json");
    if (request.method === "POST" && request.url.endsWith("/turns")) {
      state.startAttempts++;
      if (state.failStart) {
        response.statusCode = 503;
        response.end(JSON.stringify({ error: { message: "start rejected" } }));
      } else {
        const id = `accepted-${state.startAttempts}`;
        state.accepted.push(id);
        const turn = { id, thread_id: threadId };
        const operationKey = JSON.parse(text).operation_key;
        if (operationKey) state.operations.set(operationKey, turn);
        response.end(JSON.stringify({ turn }));
      }
    } else if (request.url.includes("/turn-operations/")) {
      const turn = state.operations.get(decodeURIComponent(request.url.split("/").at(-1)));
      response.statusCode = turn ? 200 : 404;
      response.end(JSON.stringify(turn || { error: { message: "operation not found" } }));
    } else if (request.method === "POST") {
      state.posts.push({ route: request.url, body: JSON.parse(text) });
      response.statusCode = state.acceptDecision ? 200 : 503;
      response.end(JSON.stringify(state.acceptDecision ? { ok: true } : { error: { message: "fixture unavailable" } }));
    } else {
      response.end(JSON.stringify({ id: threadId, pending_approvals: state.pending, turns: state.turns, latest_seq: 0 }));
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const runtimeUrl = `http://127.0.0.1:${server.address().port}`;
  const runtime = createRuntimeClient({ runtimeUrl, runtimeToken: "bridge-runtime-fixture-token" });
  try { await fn({ ...runtime, runtimeUrl, state }); }
  finally { await new Promise((resolve) => server.close(resolve)); }
}
function environment(store, runtime, extra = {}) {
  return {
    threadStore: store, runtimeJson: runtime.runtimeJson, decideRuntimeApproval: decideApproval,
    settleStoredApproval, ApprovalOwnershipError, incomingIdentity: wecomIdentity,
    sendText: async () => {}, replyText: async () => {}, sendTurnText: async () => {},
    parseApprovalDecisionArgs: () => { throw new Error("fixture uses actual parsed approval id"); },
    ensureThread: async () => store.getChat(chatId), activeTurnBlock, latestRunningTurn,
    admitTurn, createHash, isAllowed, isGroupChat, botAccountId: "fixture-bot", activeTurnTasks: new Map(),
    runtimeCapabilities: { turn_operation_idempotency: true, turn_operation_lookup: true },
    startTrackedTurnStream: () => {}, delay: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    config: { runtimeUrl: runtime.runtimeUrl, turnTimeoutMs: 1000, maxReplyChars: 10000, allowGroups: true, allowUnlisted: true, allowlist: [] },
    controlKeyboard: () => ({}), activeTurnKeyboard: () => ({}), helpText: () => "",
    clearActiveTurn: () => store.patchChat(chatId, { activeTurnId: null }), stopping: false,
    streamTurnEvents: async (chat, thread, turn) => {
      const reopened = await ThreadStore.open(store.filePath, { actions: true });
      assert.ok(reopened.turnOrigin(chat, thread, turn), "origin must be durable before delivery");
      return true;
    }, ...extra
  };
}
let incomingNumber = 0;
async function invokeStart(bridge, functions, user = "alice") {
  if (bridge === "telegram-bridge") {
    await functions.startPromptTurn(chatId, "prompt", { ...identities[bridge](user), messageKey: `message-${++incomingNumber}` });
    return functions.runPrompt(chatId);
  }
  return functions.runPrompt(chatId, "prompt", identities[bridge](user));
}

test("approval tokens retain 128-bit entropy and require their accepted human and turn", async () => {
  await withStore(async (store) => {
    await store.recordTurnOrigin(chatId, threadId, turnId, owner.actorId);
    const tokens = new Set(await Promise.all(Array.from({ length: 20 }, () => store.putAction({ kind: "approval", approvalId: "approval-a" }, owner))));
    assert.equal(tokens.size, 20);
    for (const token of tokens) assert.match(token, /^[a-f0-9]{32}$/);
    const token = [...tokens][0];
    for (const wrong of [null, { chatId }, { ...owner, chatId: "other" }, { ...owner, threadId: "other" },
      { ...owner, actorId: "" }, { ...owner, actorId: "telegram-user:bob" }, { ...owner, turnId: "other" }]) {
      assert.equal(await store.takeAction(token, wrong), null);
    }
    assert.equal((await store.takeAction(token, owner)).approvalId, "approval-a");
    assert.equal(await store.getAction(token, owner), null);
    await assert.rejects(store.putAction({ kind: "approval" }, { chatId, threadId }), ApprovalOwnershipError);
  });
});

test("legacy privileged tokens missing thread, turn or human refuse after reload", async () => {
  await withStore(async (store) => {
    await store.recordTurnOrigin(chatId, threadId, turnId, owner.actorId);
    for (const binding of [undefined, { chatId }, { chatId, threadId }, { chatId, threadId, turnId }]) {
      const token = String(Object.keys(store.data.actions).length);
      store.data.actions[token] = { kind: "approval", owner: binding, approvalId: "approval-a", createdAt: new Date().toISOString() };
    }
    await store.save();
    const reopened = await ThreadStore.open(store.filePath, { actions: true });
    for (const token of Object.keys(reopened.data.actions)) assert.equal(await reopened.getAction(token, owner), null);
  });
});

test("origins are immutable, concurrent, bounded and retired tokens never acquire newer humans", async () => {
  await withStore(async (store) => {
    await Promise.all([
      store.recordTurnOrigin(chatId, threadId, turnId, owner.actorId),
      store.recordTurnOrigin(chatId, threadId, "turn-b", "telegram-user:bob")
    ]);
    assert.equal(store.turnOrigin(chatId, threadId, turnId).actorId, owner.actorId);
    assert.equal(store.turnOrigin(chatId, threadId, "turn-b").actorId, "telegram-user:bob");
    const token = await store.putAction({ kind: "approval", approvalId: "approval-a" }, owner);
    await assert.rejects(store.recordTurnOrigin(chatId, threadId, turnId, "telegram-user:bob"), ApprovalOwnershipError);
    await store.recordTurnOrigin(chatId, threadId, "turn-c", "telegram-user:bob");
    const reopened = await ThreadStore.open(store.filePath, { actions: true, actionLimit: 2 });
    assert.equal((await reopened.getChat(chatId)).turnOrigins.length, 2);
    assert.equal(reopened.turnOrigin(chatId, threadId, turnId), null);
    assert.equal(await reopened.getAction(token, owner), null);
    await reopened.setChat(chatId, { threadId: "other" });
    assert.equal(reopened.turnOrigin(chatId, threadId, "turn-c"), null);
    await reopened.setChat(chatId, { threadId });
    assert.equal(reopened.turnOrigin(chatId, threadId, "turn-c"), null);
  }, { actionLimit: 2 });
});

test("shared decisions require Runtime pending turn and originating human, not current sender", async () => {
  await withRuntime(async (runtime) => withStore(async (store) => {
    await store.recordTurnOrigin(chatId, threadId, turnId, owner.actorId);
    const args = { store, chatId, actorId: owner.actorId, approvalId: "approval-a", decision: "allow" };
    for (const override of [{ actorId: "" }, { actorId: "telegram-user:bob" }, { approvalId: "approval-b" },
      { chatId: "other" }, { turnId: "turn-b" }]) {
      await assert.rejects(decideApproval(runtime.runtimeJson, { ...args, ...override }), ApprovalOwnershipError);
    }
    runtime.state.pending = [{ id: "approval-a" }];
    await assert.rejects(decideApproval(runtime.runtimeJson, args), ApprovalOwnershipError);
    runtime.state.pending = [{ id: "approval-a", turn_id: "turn-b" }];
    await assert.rejects(decideApproval(runtime.runtimeJson, args), ApprovalOwnershipError);
    assert.equal(runtime.state.posts.length, 0);
    runtime.state.pending = [{ id: "approval-a", turn_id: turnId }];
    await decideApproval(runtime.runtimeJson, { ...args, decision: "deny", remember: "yes" });
    assert.deepEqual(runtime.state.posts, [{ route: "/v1/approvals/approval-a", body: { decision: "deny", remember: false } }]);
  }));
});

test("thread switch during Runtime read refuses before the approval POST", async () => {
  await withStore(async (store) => {
    await store.recordTurnOrigin(chatId, threadId, turnId, owner.actorId);
    const runtimeJson = async (route, options) => {
      assert.equal(options, undefined, "must not post after switch");
      await store.setChat(chatId, { threadId: "other" });
      return { pending_approvals: [{ id: "approval-a", turn_id: turnId }] };
    };
    await assert.rejects(decideApproval(runtimeJson, { store, chatId, actorId: owner.actorId, approvalId: "approval-a", decision: "allow" }), ApprovalOwnershipError);
  });
});

for (const bridge of Object.keys(identities)) {
  test(`${bridge} shipping decision refuses unrelated, missing and other originating human over Runtime HTTP`, async () => {
    await withRuntime(async (runtime) => withStore(async (store) => {
      await store.recordTurnOrigin(chatId, threadId, turnId, actors[bridge]);
      await store.patchChat(chatId, { authorizedIdentity: identities[bridge]("bob"), activeTurnId: "later-turn" });
      const reopened = await ThreadStore.open(store.filePath, { actions: true });
      const { decideApproval: consumer } = await shipping(bridge, ["turnActor", "decideApproval"], environment(reopened, runtime,
        bridge === "weixin-bridge" ? { botAccount: { accountId: "bot-a" } } : {}));
      const action = { decision: "allow", approvalId: "approval-a", remember: true };
      await consumer(chatId, { ...action, approvalId: "approval-b" }, identities[bridge]("alice"));
      if (bridge !== "weixin-bridge") {
        await consumer(chatId, action, identities[bridge]("bob"));
        await consumer(chatId, action, identities[bridge](""));
      }
      assert.equal(runtime.state.posts.length, 0);
      await consumer(chatId, action, identities[bridge]("alice"));
      assert.deepEqual(runtime.state.posts, [{ route: "/v1/approvals/approval-a", body: { decision: "allow", remember: bridge !== "wecom-bridge" } }]);
    }));
  });
}

test("Weixin shipping decision rejects replacement or missing account identity without transferring the origin", async () => {
  await withRuntime(async (runtime) => withStore(async (store) => {
    await store.recordTurnOrigin(chatId, threadId, turnId, actors["weixin-bridge"]);
    const reopened = await ThreadStore.open(store.filePath, { actions: true });
    const botAccount = { accountId: "bot-a" };
    const { decideApproval: consumer } = await shipping("weixin-bridge", ["turnActor", "decideApproval"],
      environment(reopened, runtime, { botAccount }));
    const action = { decision: "allow", approvalId: "approval-a", remember: true };
    for (const accountId of ["bot-b", "", null]) {
      botAccount.accountId = accountId;
      await consumer(chatId, action);
      assert.equal(runtime.state.posts.length, 0);
      assert.equal(reopened.turnOrigin(chatId, threadId, turnId).actorId, actors["weixin-bridge"]);
    }
    botAccount.accountId = "bot-a";
    await consumer(chatId, action);
    assert.deepEqual(runtime.state.posts, [{ route: "/v1/approvals/approval-a", body: { decision: "allow", remember: true } }]);
  }));
});

test("Weixin shipping decision refuses a legacy origin without bot account provenance after reload", async () => {
  await withRuntime(async (runtime) => withStore(async (store) => {
    await store.recordTurnOrigin(chatId, threadId, turnId, "weixin-user:chat-a");
    const reopened = await ThreadStore.open(store.filePath, { actions: true });
    const { decideApproval: consumer } = await shipping("weixin-bridge", ["turnActor", "decideApproval"],
      environment(reopened, runtime, { botAccount: { accountId: "bot-a" } }));
    await consumer(chatId, { decision: "allow", approvalId: "approval-a", remember: true });
    assert.equal(runtime.state.posts.length, 0);
    assert.equal(reopened.turnOrigin(chatId, threadId, turnId).actorId, "weixin-user:chat-a");
  }));
});

for (const bridge of ["telegram-bridge", "feishu-bridge", "wecom-bridge"]) {
  test(`${bridge} accepted start binds before delivery; failed, queued and later starts cannot transfer an old approval`, async () => {
    await withRuntime(async (runtime) => withStore(async (store) => {
      const functions = await shipping(bridge, ["turnActor", "runPrompt", "decideApproval", ...(bridge === "telegram-bridge" ? ["startPromptTurn"] : [])], environment(store, runtime, bridge === "wecom-bridge" ? {
        streamTurnEvents: async (chat, frame, thread, turn) => {
          const reopened = await ThreadStore.open(store.filePath, { actions: true });
          assert.ok(reopened.turnOrigin(chat, thread, turn), "origin must be durable before delivery");
        }
      } : {}));
      await invokeStart(bridge, functions, "alice");
      const first = runtime.state.accepted[0];
      assert.equal(store.turnOrigin(chatId, threadId, first).actorId, actors[bridge]);
      runtime.state.pending = [{ id: "approval-a", turn_id: first }];
      await store.patchChat(chatId, { authorizedIdentity: identities[bridge]("bob") });
      runtime.state.failStart = true;
      await assert.rejects(invokeStart(bridge, functions, "bob"), /start rejected/);
      assert.equal(store.turnOrigin(chatId, threadId, first).actorId, actors[bridge]);
      runtime.state.failStart = false;
      runtime.state.turns = [{ id: first, status: "queued" }];
      const before = runtime.state.startAttempts;
      await invokeStart(bridge, functions, "bob");
      assert.equal(runtime.state.startAttempts, before);
      runtime.state.turns = [];
      await invokeStart(bridge, functions, "bob");
      assert.equal(store.turnOrigin(chatId, threadId, first).actorId, actors[bridge]);
      const reopened = await ThreadStore.open(store.filePath, { actions: true });
      const { decideApproval: consumer } = await shipping(bridge, ["turnActor", "decideApproval"], environment(reopened, runtime));
      await consumer(chatId, { decision: "allow", approvalId: "approval-a" }, identities[bridge]("bob"));
      assert.equal(runtime.state.posts.length, 0);
      await consumer(chatId, { decision: "allow", approvalId: "approval-a" }, identities[bridge]("alice"));
      assert.equal(runtime.state.posts.length, 1);
    }));
  });
}

test("Telegram shipping SSE mints a bound button and actual callback human controls retry, restart and replay", async () => {
  await withRuntime(async (runtime) => withStore(async (store) => {
    await store.recordTurnOrigin(chatId, threadId, turnId, owner.actorId);
    runtime.state.events = [{ seq: 1, turn_id: turnId, event: "approval.required", payload: { approval_id: "approval-a" } }];
    const telegram = await import("../../telegram-bridge/src/lib.mjs");
    const env = environment(store, runtime, {
      authHeaders: runtime.authHeaders, readSse, readJsonSafe, compactRuntimeError,
      sendTypingAction: async () => {}, TYPING_INTERVAL_MS: 2000, LAST_SEQ_FLUSH_INTERVAL_MS: 2000,
      approvalKeyboard: telegram.approvalKeyboard
    });
    const { streamTurnEvents } = await shipping("telegram-bridge", ["streamTurnEvents"], env);
    await streamTurnEvents(chatId, threadId, turnId, 0);
    const [token] = Object.keys(store.data.actions);
    assert.ok(token, "actual SSE must mint a token");
    assert.deepEqual(store.data.actions[token].owner, owner);
    const invoke = async (current, user) => {
      const values = environment(current, runtime, {
        ...telegram, config: { ...env.config, allowGroups: true, allowUnlisted: true, allowlist: [] },
        answerCallback: async () => {}, editMessageReplyMarkup: async () => {},
        rememberAuthorizedIdentity: async (identity) => current.patchChat(chatId, { authorizedIdentity: identity })
      });
      const { handleCallbackQuery } = await shipping("telegram-bridge",
        ["turnActor", "decideApproval", "handleStoredAction", "handleModalAction", "handleCallbackQuery"], values);
      return handleCallbackQuery({ id: "callback-fixture", data: telegram.approvalKeyboard(token).inline_keyboard[0][0].callback_data,
        from: { id: user }, message: { message_id: 1, chat: { id: chatId, type: "group" } } });
    };
    await invoke(store, "bob");
    assert.equal(runtime.state.posts.length, 0);
    // The same approval id cannot authorize a different accepted turn, even
    // when the same human happens to have initiated both turns.
    await store.recordTurnOrigin(chatId, threadId, "other-turn", owner.actorId);
    runtime.state.pending = [{ id: "approval-a", turn_id: "other-turn" }];
    await invoke(store, "alice");
    assert.equal(runtime.state.posts.length, 0);
    runtime.state.pending = [{ id: "approval-a", turn_id: turnId }];
    runtime.state.acceptDecision = false;
    await assert.rejects(invoke(store, "alice"), /fixture unavailable/);
    const reopened = await ThreadStore.open(store.filePath, { actions: true });
    assert.ok(await reopened.getAction(token, owner));
    runtime.state.acceptDecision = true;
    await invoke(reopened, "alice");
    const accepted = await ThreadStore.open(store.filePath, { actions: true });
    assert.equal(await accepted.getAction(token, owner), null);
    await invoke(accepted, "alice");
    assert.equal(runtime.state.posts.length, 2, "failed then accepted; accepted replay never posts");
  }));
});

test("Telegram and Feishu actual dispatch passes the admitted human to prompt and text approval", async () => {
  for (const bridge of ["telegram-bridge", "feishu-bridge"]) {
    const lib = await import(`../../${bridge}/src/lib.mjs`);
    const calls = [];
    const identity = identities[bridge]("alice");
    const values = { ...lib, sendText: async () => {},
      decideApproval: async (...args) => calls.push(args),
      startPromptTurn: (...args) => calls.push(args),
      runPrompt: async (...args) => calls.push(args) };
    const { handleCommand } = await shipping(bridge, ["handleCommand"], values);
    await handleCommand(chatId, lib.parseCommand("/allow approval-a"), identity);
    await handleCommand(chatId, lib.parseCommand("hello"), identity);
    assert.equal(calls.length, 2, bridge);
    assert.equal(calls[0][2], identity, bridge);
    assert.equal(calls[1][2], identity, bridge);
  }
});


test("Telegram and Feishu shipping inbound events retain the actual admitted human through accepted start", async () => {
  for (const bridge of ["telegram-bridge", "feishu-bridge"]) {
    await withRuntime(async (runtime) => withStore(async (store) => {
      const lib = await import(`../../${bridge}/src/lib.mjs`);
      let delivered;
      const delivery = new Promise((resolve) => { delivered = resolve; });
      let markCleared;
      const activeCleared = new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`${bridge} did not durably clear its accepted turn`)), 5000);
        markCleared = () => { clearTimeout(timer); resolve(); };
      });
      const patchChat = store.patchChat.bind(store);
      store.patchChat = async (chat, patch) => {
        const result = await patchChat(chat, patch);
        if (patch?.activeTurnId === null && runtime.state.accepted.length) {
          const reopened = await ThreadStore.open(store.filePath, { actions: true });
          assert.equal((await reopened.getChat(chat)).activeTurnId, null, `${bridge} clear must be durable before fixture cleanup`);
          markCleared();
        }
        return result;
      };
      const values = environment(store, runtime, {
        ...lib, activeTurnTasks: new Map(),
        config: { allowGroups: true, allowUnlisted: false, allowlist: [chatId], requirePrefixInGroup: false },
        streamTurnEvents: async (chat, thread, turn) => {
          const reopened = await ThreadStore.open(store.filePath, { actions: true });
          assert.equal((await reopened.getChat(chat)).activeTurnId, turn, `${bridge} must persist the accepted turn before streaming`);
          assert.equal(reopened.turnOrigin(chat, thread, turn).actorId, actors[bridge]);
          delivered();
          return true;
        }
      });
      const names = bridge === "telegram-bridge" ?
        ["turnActor", "runPrompt", "startPromptTurn", "startTrackedTurnStream", "handleCommand", "rememberAuthorizedIdentity", "handleIncomingUpdate"] :
        ["turnActor", "runPrompt", "handleCommand", "handleIncomingMessage"];
      const handlers = await shipping(bridge, names, values);
      if (bridge === "telegram-bridge") {
        await handlers.handleIncomingUpdate({ update_id: 1, message: { message_id: 1, text: "hello",
          chat: { id: chatId, type: "group" }, from: { id: "alice" } } });
        await delivery;
      } else {
        await handlers.handleIncomingMessage({ sender: { sender_id: { open_id: "alice" } },
          message: { chat_id: chatId, chat_type: "group", message_id: "message-a", message_type: "text",
            content: JSON.stringify({ text: "hello" }) } });
      }
      await delivery;
      await activeCleared;
      assert.equal(runtime.state.accepted.length, 1);
      assert.equal(store.turnOrigin(chatId, threadId, runtime.state.accepted[0]).actorId, actors[bridge]);
    }));
  }
});

test("WeCom shipping SSE and natural approval callback use durable Runtime-turn origin, not the hint's latest sender", async () => {
  await withRuntime(async (runtime) => withStore(async (store) => {
    const lib = await import("../../wecom-bridge/src/lib.mjs");
    await store.recordTurnOrigin(chatId, threadId, turnId, actors["wecom-bridge"]);
    runtime.state.events = [{ seq: 1, turn_id: turnId, event: "approval.required", payload: { approval_id: "approval-a" } }];
    const pendingApprovals = new Map();
    const values = environment(store, runtime, {
      ...lib, authHeaders: runtime.authHeaders, readSse, readJsonSafe, compactRuntimeError,
      config: { runtimeUrl: runtime.runtimeUrl, turnTimeoutMs: 1000, approvalTimeoutMs: 10000 },
      pendingApprovals, generateReqId: () => "stream-fixture", client: { replyStream: async () => {} }
    });
    const { streamTurnEvents } = await shipping("wecom-bridge", ["streamTurnEvents"], values);
    await streamTurnEvents(chatId, identities["wecom-bridge"]("bob"), threadId, turnId, 0);
    assert.equal(pendingApprovals.get(chatId).approvalId, "approval-a");
    const reopened = await ThreadStore.open(store.filePath, { actions: true });
    const { handleCommand } = await shipping("wecom-bridge", ["turnActor", "decideApproval", "handleCommand"],
      { ...values, threadStore: reopened });
    await handleCommand(chatId, lib.parseCommand("允许"), identities["wecom-bridge"]("bob"));
    assert.equal(runtime.state.posts.length, 0);
    assert.ok(pendingApprovals.has(chatId), "refused member must not retire the legitimate pending hint");
    await handleCommand(chatId, lib.parseCommand("允许"), identities["wecom-bridge"]("alice"));
    assert.equal(runtime.state.posts.length, 1);
    assert.deepEqual(runtime.state.posts[0].body, { decision: "allow", remember: false });
    assert.equal(pendingApprovals.has(chatId), false);
  }));
});


test("retired origin refuses stale pending approval even when newer turns have the same human", async () => {
  await withRuntime(async (runtime) => withStore(async (store) => {
    await store.recordTurnOrigin(chatId, threadId, turnId, owner.actorId);
    const token = await store.putAction({ kind: "approval", approvalId: "approval-a" }, owner);
    await store.recordTurnOrigin(chatId, threadId, "newer-1", owner.actorId);
    await store.recordTurnOrigin(chatId, threadId, "newer-2", owner.actorId);
    const reopened = await ThreadStore.open(store.filePath, { actions: true, actionLimit: 2 });
    assert.equal(await reopened.getAction(token, owner), null);
    await assert.rejects(decideApproval(runtime.runtimeJson, { store: reopened, chatId, actorId: owner.actorId,
      approvalId: "approval-a", decision: "allow" }), ApprovalOwnershipError);
    assert.equal(runtime.state.posts.length, 0);
  }, { actionLimit: 2 }));
});

test("existing action TTL is enforced on lookup after restart", async () => {
  await withStore(async (store) => {
    await store.recordTurnOrigin(chatId, threadId, turnId, owner.actorId);
    const token = await store.putAction({ kind: "approval", approvalId: "approval-a" }, owner);
    store.data.actions[token].createdAt = new Date(0).toISOString();
    await store.save();
    const reopened = await ThreadStore.open(store.filePath, { actions: true });
    assert.equal(await reopened.getAction(token, owner), null);
  });
});
