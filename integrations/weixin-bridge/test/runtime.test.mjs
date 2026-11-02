import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const entry = fileURLToPath(new URL("../src/index.mjs", import.meta.url));
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check, message, timeout = 8000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { if (await check()) return; await delay(25); }
  assert.fail(message);
}
function incoming(id, text, user = "alice", contextToken = "context-fixture") {
  return { message_id: id, from_user_id: user, message_type: 1, context_token: contextToken, item_list: [{ type: 1, text_item: { text } }] };
}
async function fixture(t, options = {}) {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "weixin-runtime-"));
  const state = { polls: 0, batches: [], posts: [], sent: [], summaryQueries: [], lookups: 0, threadCreates: 0, threadReads: [], streams: new Set(), turns: [], items: [], seq: 0, approvals: [], decisions: [], logs: "" };
  const json = (response, body, status = 200) => { response.writeHead(status, { "Content-Type": "application/json" }); response.end(JSON.stringify(body)); };
  const server = http.createServer(async (req, res) => {
    let raw = ""; for await (const chunk of req) raw += chunk;
    const body = raw ? JSON.parse(raw) : {};
    const url = new URL(req.url, "http://fixture");
    if (url.pathname.endsWith("/getupdates")) {
      state.polls++;
      await delay(35);
      if (res.destroyed) return;
      return json(res, { ret: 0, msgs: state.batches.shift() || [], get_updates_buf: String(state.polls) });
    }
    if (url.pathname.endsWith("/sendmessage")) {
      state.sent.push(body.msg);
      if (options.send) return options.send(res, body.msg, state, json);
      return json(res, { ret: 0, message_id: String(state.sent.length) });
    }
    if (url.pathname.includes("/msg/notify")) return json(res, { ret: 0 });
    if (url.pathname === "/v1/runtime/info") return json(res, { version: "fixture", capabilities: options.capabilities ?? { turn_operation_idempotency: true, turn_operation_lookup: true } });
    if (url.pathname === "/health") return json(res, { status: "ok" });
    if (url.pathname === "/v1/workspace/status") return json(res, { workspace: dir });
    if (url.pathname === "/v1/threads" && req.method === "POST") {
      state.threadRequest = body;
      return json(res, { id: `thread-${++state.threadCreates}` });
    }
    if (url.pathname === "/v1/threads/summary") { state.summaryQueries.push((url.searchParams.get("thread_ids") || "").split(",").filter(Boolean)); return json(res, options.summaries || []); }
    if (/^\/v1\/threads\/[^/]+$/.test(url.pathname)) {
      state.threadReads.push(url.pathname);
      return json(res, { id: url.pathname.split("/").at(-1), latest_seq: state.seq, turns: state.turns, items: state.items, pending_approvals: state.approvals });
    }
    if (url.pathname === "/v1/threads/thread-1/turns") {
      state.posts.push(body);
      const turn = { id: "turn-1", thread_id: "thread-1", status: "in_progress" };
      state.turns = [turn];
      if (options.admission) return options.admission(req, res, turn, state, json);
      return json(res, { turn }, 201);
    }
    if (url.pathname.includes("/turn-operations/")) {
      state.lookups++;
      if (options.lookup) return options.lookup(res, state, json);
      return json(res, state.turns[0] || {}, state.turns.length ? 200 : 404);
    }
    if (url.pathname === "/v1/threads/thread-1/events") {
      res.writeHead(200, { "Content-Type": "text/event-stream" }); res.flushHeaders();
      state.streams.add(res); res.on("close", () => state.streams.delete(res));
      return;
    }
    if (url.pathname === "/v1/approvals/approval-1") { state.decisions.push(body); state.approvals = []; return json(res, {}); }
    return json(res, { error: url.pathname }, 404);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const baseUrl = `http://127.0.0.1:${server.address().port}`;
  const account = { accountId: "bot-A", token: "fixture-bot-token", baseUrl };
  await fs.writeFile(path.join(dir, "account.json"), JSON.stringify(account));
  const children = [];
  state.start = (extra = {}) => {
    const child = spawn(process.execPath, [entry], { env: { ...process.env, CODEWHALE_RUNTIME_URL: baseUrl, CODEWHALE_RUNTIME_TOKEN: "fixture-runtime-token", CODEWHALE_WORKSPACE: dir, WEIXIN_STATE_DIR: dir, WEIXIN_CHAT_ALLOWLIST: "alice,bob", CODEWHALE_TURN_TIMEOUT_MS: "1500", ...extra }, stdio: ["ignore", "pipe", "pipe"] });
    children.push(child);
    child.stdout.on("data", (data) => { state.logs += data; });
    child.stderr.on("data", (data) => { state.logs += data; });
    return child;
  };
  state.disk = async () => { try { return JSON.parse(await fs.readFile(path.join(dir, "thread-map.json"), "utf8")); } catch { return {}; } };
  state.event = (event, payload, turnId = "turn-1") => {
    state.seq++;
    const record = { event, payload, thread_id: "thread-1", turn_id: turnId, seq: state.seq };
    for (const response of state.streams) response.write(`data: ${JSON.stringify(record)}\n\n`);
  };
  state.complete = (text, status = "completed") => {
    state.items = [{ turn_id: "turn-1", kind: "agent_message", detail: text, summary: "truncated" }];
    state.turns[0].status = status;
    state.event("turn.completed", { turn: state.turns[0] });
  };
  state.dir = dir; state.account = account;
  t.after(async () => {
    for (const child of children) child.kill("SIGKILL");
    for (const response of state.streams) response.destroy();
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
    await fs.rm(dir, { recursive: true, force: true });
  });
  return state;
}

test("production process keeps polling and owner approvals live, dedupes real message IDs", async (t) => {
  const f = await fixture(t); f.batches.push([incoming(0, "private original prompt"), incoming(undefined, "must not run")]); f.start();
  await until(() => f.streams.size, "accepted turn did not start observation");
  const polls = f.polls;
  f.approvals = [{ id: "approval-1", turn_id: "turn-1", tool_name: "fixture-tool" }];
  f.event("approval.required", f.approvals[0]);
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text.includes("fixture-tool")), "approval notice missing");
  const code = f.sent.find((msg) => msg.item_list[0].text_item.text.includes("fixture-tool")).item_list[0].text_item.text.match(/Reply 1 ([a-z2-9]{6}) to allow/)[1];
  f.batches.push([incoming(0, "private original prompt"), incoming(2, "/allow approval-1", "bob"), incoming(3, `1 ${code}`)]);
  await until(() => f.decisions.length === 1 && f.polls > polls, "initiating owner could not approve or polling stopped during stream");
  assert.equal(f.posts.length, 1); assert.ok(f.polls > polls); assert.deepEqual(f.decisions, [{ decision: "allow", remember: false }]);
  f.event("item.delta", { kind: "agent_message", delta: "answer" }); f.complete("answer");
  await until(async () => !(await f.disk()).chats?.alice?.activeTurnId, "accepted output did not settle");
  const disk = await f.disk(); assert.ok(disk.messages.length >= 3); assert.equal(disk.chats.alice.contextToken, "context-fixture");
  assert.equal(f.logs.includes("private original prompt"), false);
});

test("restart reattaches exact accepted turn and preserves accumulator across EOF", async (t) => {
  const f = await fixture(t); f.batches.push([incoming(1, "work")]); const first = f.start();
  await until(() => f.streams.size, "first stream missing"); f.event("item.delta", { kind: "agent_message", delta: "first " });
  await until(async () => (await f.disk()).chats?.alice?.turnDelivery?.responseText === "first ", "delta was not durable");
  for (const response of f.streams) response.end();
  await delay(75); assert.equal((await f.disk()).chats.alice.activeTurnId, "turn-1");
  first.kill("SIGKILL"); await new Promise((resolve) => first.once("exit", resolve));
  f.items = [{ turn_id: "turn-1", kind: "agent_message", detail: "first second" }]; f.turns[0].status = "completed"; f.seq++;
  f.start(); await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text === "first second"), "restart lost final output");
  assert.equal(f.posts.length, 1); assert.equal(f.sent.filter((msg) => msg.item_list[0].text_item.text === "first second").length, 1);
});

test("ambiguous admission uses stable operation lookup rather than a duplicate POST", async (t) => {
  const f = await fixture(t, { admission: (_req, response) => response.destroy() }); f.batches.push([incoming(1, "work")]); f.start();
  await until(() => f.streams.size, "operation lookup did not recover accepted turn");
  assert.equal(f.posts.length, 1); assert.equal(f.lookups, 1); assert.match(f.posts[0].operation_key, /^[a-f0-9]{64}$/);
  assert.equal((await f.disk()).chats.alice.activeTurnId, "turn-1");
});

test("proven rejection holds the output until the member asks to continue; uncertain send is retained without resending", async (t) => {
  const f = await fixture(t, { send: (response, msg, state, json) => {
    if (state.sent.length === 1) return json(response, { ret: -2 });
    return response.destroy();
  } }); f.batches.push([incoming(1, "work")]); f.start();
  await until(() => f.streams.size, "stream missing"); f.event("item.delta", { kind: "agent_message", delta: "retained answer" }); f.complete("retained answer");
  await until(async () => (await f.disk()).chats?.alice?.turnDelivery?.outputs?.[0]?.status === "held", "rejected send was not held");
  await delay(1200); assert.equal(f.sent.length, 1);
  f.batches.push([incoming(2, "继续")]);
  await until(async () => (await f.disk()).chats?.alice?.turnDelivery?.outputs?.[0]?.status === "uncertain", "uncertain send was discarded");
  assert.equal(f.sent.length, 2); assert.equal(f.sent[0].client_id, f.sent[1].client_id);
  await delay(1200); assert.equal(f.sent.length, 2); assert.equal((await f.disk()).chats.alice.activeTurnId, "turn-1");
});

test("changed bot account cannot reuse or flush a retained old conversation", async (t) => {
  const f = await fixture(t); f.batches.push([incoming(1, "work")]); const first = f.start();
  await until(() => f.streams.size, "stream missing"); first.kill("SIGKILL"); await new Promise((resolve) => first.once("exit", resolve));
  f.items = [{ turn_id: "turn-1", kind: "agent_message", detail: "old private answer" }]; f.turns[0].status = "completed";
  await fs.writeFile(path.join(f.dir, "account.json"), JSON.stringify({ ...f.account, accountId: "bot-B" }));
  f.batches.push([incoming(2, "new account prompt")]); f.start();
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text.includes("pending turn or retained reply")), "replacement account prompt was not handled");
  assert.equal(f.posts.length, 1); assert.equal(f.sent.some((msg) => msg.item_list[0].text_item.text.includes("old private answer")), false);
  assert.equal(f.sent.find((msg) => msg.item_list[0].text_item.text.includes("pending turn or retained reply")).context_token, undefined);
  assert.equal((await f.disk()).chats.alice.turnDelivery.accountId, "bot-A");
});

test("unattributed legacy context token is not reused for an outgoing message", async (t) => {
  const f = await fixture(t);
  await fs.writeFile(path.join(f.dir, "thread-map.json"), JSON.stringify({
    chats: { alice: { bindingAccountId: "bot-A", contextToken: "legacy-context" } },
    messages: [], inflight: {},
  }));
  f.batches.push([incoming(1, "/help", "alice", "")]); f.start();
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text.includes("显示此帮助")), "help response was not sent");
  assert.equal(f.sent.at(-1).context_token, undefined);
});

test("both Engine capability flags are required before reconciling an uncertain admission", async (t) => {
  const f = await fixture(t, { capabilities: { turn_operation_idempotency: true, turn_operation_lookup: false }, admission: (_req, response) => response.destroy() });
  f.batches.push([incoming(1, "legacy admission")]); f.start();
  await until(async () => (await f.disk()).chats?.alice?.pendingAdmission?.submitted, "uncertain request was not retained");
  const polls = f.polls; await until(() => f.polls > polls + 3, "uncertain admission stopped inbound polling");
  await delay(1100); assert.equal(f.posts.length, 1); assert.equal(f.lookups, 0); assert.equal(f.posts[0].operation_key, undefined);
  assert.equal((await f.disk()).chats.alice.pendingAdmission.request.prompt, "legacy admission");
});

test("pre-admission inflight claim recovers through the actual production store", async (t) => {
  const f = await fixture(t); const key = JSON.stringify(["bot-A", "alice", "1"]);
  await fs.writeFile(path.join(f.dir, "thread-map.json"), JSON.stringify({ chats: {}, messages: [key], inflight: { [key]: { at: "fixture" } } }));
  f.batches.push([incoming(1, "recover claimed prompt")]); f.start();
  await until(() => f.streams.size, "inflight prompt was silently skipped");
  assert.equal(f.posts.length, 1); assert.equal((await f.disk()).inflight[key], undefined);
  assert.equal((await f.disk()).chats.alice.turnDelivery.messageKey, key);
});

test("pending new-thread and turn descriptors stay frozen across restart configuration changes", async (t) => {
  const f = await fixture(t); const key = JSON.stringify(["bot-A", "alice", "1"]);
  const request = { prompt: "frozen prompt", input_summary: "frozen prompt", model: "frozen-model", mode: "agent", allow_shell: false, trust_mode: false, auto_approve: false, operation_key: "frozen-operation" };
  const threadRequest = { model: "frozen-model", workspace: "/frozen/workspace", mode: "agent", allow_shell: false, trust_mode: false, auto_approve: false, archived: false, system_prompt: "frozen system prompt" };
  await fs.writeFile(path.join(f.dir, "thread-map.json"), JSON.stringify({ chats: { alice: { pendingAdmission: { accountId: "bot-A", actorId: "weixin-account:bot-A:user:alice", messageKey: key, request, threadRequest, operationKey: request.operation_key, threadId: null, submitted: false, sinceSeq: null } } }, messages: [], inflight: {} }));
  f.start({ CODEWHALE_WORKSPACE: "/changed/workspace", CODEWHALE_MODEL: "changed-model", CODEWHALE_ALLOW_SHELL: "true" });
  await until(() => f.streams.size, "pending request did not recover");
  assert.deepEqual(f.threadRequest, threadRequest); assert.deepEqual(f.posts[0], request); assert.equal(f.posts.length, 1);
});

test("completed accepted conversation is not reused by a replacement bot account", async (t) => {
  const f = await fixture(t); f.batches.push([incoming(1, "work")]); const first = f.start();
  await until(() => f.streams.size, "stream missing"); f.event("item.delta", { kind: "agent_message", delta: "old answer" }); f.complete("old answer");
  await until(async () => (await f.disk()).chats?.alice?.turnDelivery?.terminal && !(await f.disk()).chats.alice.activeTurnId, "old turn did not settle");
  first.kill("SIGKILL"); await new Promise((resolve) => first.once("exit", resolve));
  await fs.writeFile(path.join(f.dir, "account.json"), JSON.stringify({ ...f.account, accountId: "bot-B" }));
  f.batches.push([incoming(2, "replacement prompt")]); f.start();
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text.includes("another or unverified bot account")), "completed old binding was silently reused");
  assert.equal(f.posts.length, 1); assert.equal((await f.disk()).chats.alice.bindingAccountId, "bot-A");
});

test("canceled turn retains and sends partial assistant output with terminal status", async (t) => {
  const f = await fixture(t); f.batches.push([incoming(1, "work")]); f.start();
  await until(() => f.streams.size, "stream missing"); f.event("item.delta", { kind: "agent_message", delta: "partial answer" }); f.complete("partial answer", "canceled");
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text === "partial answer\n\nTurn canceled."), "cancellation dropped partial output");
  assert.equal(f.posts.length, 1);
});

test("unanswered admission response leaves polling and status controls live", async (t) => {
  const f = await fixture(t, { admission: () => {} }); f.batches.push([incoming(1, "slow admission")]); f.start();
  await until(() => f.posts.length === 1, "admission request missing");
  const started = Date.now(); const polls = f.polls; f.batches.push([incoming(2, "/status")]);
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text.includes("user_id=alice")), "admission blocked status control until timeout", 3500);
  await until(() => f.polls > polls, "inbound polling did not advance while admission remained unanswered", 3500);
  assert.ok(Date.now() - started < 7000); assert.equal(f.posts.length, 1);
  assert.equal((await f.disk()).chats.alice.pendingAdmission.submitted, true);
});

test("replacement account explicitly starts fresh while retaining the old uncertain receipt", async (t) => {
  const f = await fixture(t);
  const old = { threadId: "old-thread", bindingAccountId: "bot-A", activeTurnId: "old-turn", contextToken: "old-context", turnDelivery: {
    accountId: "bot-A", actorId: "weixin-account:bot-A:user:alice", messageKey: "old-key", turnId: "old-turn",
    outputs: [{ text: "old private answer", status: "uncertain", clientId: "old-client" }],
  } };
  await fs.writeFile(path.join(f.dir, "thread-map.json"), JSON.stringify({ chats: { alice: old }, messages: [], inflight: {} }));
  await fs.writeFile(path.join(f.dir, "account.json"), JSON.stringify({ ...f.account, accountId: "bot-B" }));
  f.batches.push([incoming(1, "/new", "alice", "bot-B-context")]); f.start();
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text === "Created thread thread-1"), "replacement account remained blocked");
  const current = (await f.disk()).chats.alice;
  assert.equal(current.bindingAccountId, "bot-B"); assert.equal(current.activeTurnId, null); assert.equal(current.turnDelivery, undefined);
  assert.equal(current.contextToken, "bot-B-context"); assert.equal(current.contextTokenAccountId, "bot-B");
  const retained = Object.values(current.retiredAccountStates);
  assert.equal(retained.length, 1); assert.equal(retained[0].threadId, "old-thread");
  assert.deepEqual(retained[0].turnDelivery, old.turnDelivery); assert.equal(retained[0].activeTurnId, "old-turn");
  assert.equal(retained[0].contextToken, "old-context");
  assert.equal(f.sent.find((msg) => msg.item_list[0].text_item.text === "Created thread thread-1").context_token, "bot-B-context");
  await delay(1100); assert.equal(f.posts.length, 0); assert.equal(f.threadReads.length, 0);
  assert.equal(f.sent.some((msg) => msg.item_list[0].text_item.text.includes("old private answer")), false);
});

for (const receiptKind of ["pendingAdmission", "turnDelivery"]) test(`unbound ${receiptKind} keeps its account context until explicit replacement`, async (t) => {
  const f = await fixture(t);
  const receipt = { accountId: "bot-A", actorId: "weixin-account:bot-A:user:alice", messageKey: "old-key",
    ...(receiptKind === "pendingAdmission"
      ? { request: { prompt: "old private prompt" }, submitted: false }
      : { outputs: [{ text: "old private answer", status: "uncertain" }] }),
  };
  const old = { contextToken: "bot-A-context", contextTokenAccountId: "bot-A", [receiptKind]: receipt };
  await fs.writeFile(path.join(f.dir, "thread-map.json"), JSON.stringify({ chats: { alice: old }, messages: [], inflight: {} }));
  await fs.writeFile(path.join(f.dir, "account.json"), JSON.stringify({ ...f.account, accountId: "bot-B" }));
  f.batches.push([incoming(1, "/help", "alice", "bot-B-context")]); f.start();
  await until(() => f.sent.length === 1, "new account help was not handled");
  assert.equal(f.sent[0].context_token, undefined);
  assert.equal((await f.disk()).chats.alice.contextToken, "bot-A-context");
  assert.deepEqual((await f.disk()).chats.alice[receiptKind], receipt);
  f.batches.push([incoming(2, "/new", "alice", "bot-B-context")]);
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text === "Created thread thread-1"), "explicit replacement did not complete");
  const current = (await f.disk()).chats.alice;
  const retained = Object.values(current.retiredAccountStates);
  assert.equal(retained.length, 1);
  assert.equal(retained[0].contextToken, "bot-A-context");
  assert.equal(retained[0].contextTokenAccountId, "bot-A");
  assert.deepEqual(retained[0][receiptKind], receipt);
  assert.equal(current.contextToken, "bot-B-context");
  assert.equal(current.contextTokenAccountId, "bot-B");
  assert.equal(current.bindingAccountId, "bot-B");
  assert.equal(current[receiptKind], undefined);
  assert.equal(f.sent.at(-1).context_token, "bot-B-context");
  assert.equal(f.posts.length, 0);
  assert.equal(f.threadReads.length, 0);
});

test("unattributed pending admission cannot have its context replaced by an inbound token", async (t) => {
  const f = await fixture(t);
  const old = { contextToken: "unattributed-context", pendingAdmission: { request: { prompt: "retained prompt" } } };
  await fs.writeFile(path.join(f.dir, "thread-map.json"), JSON.stringify({ chats: { alice: old }, messages: [], inflight: {} }));
  f.batches.push([incoming(1, "/help")]); f.start();
  await until(() => f.sent.length === 1, "help was not handled");
  assert.equal(f.sent[0].context_token, undefined);
  assert.equal((await f.disk()).chats.alice.contextToken, old.contextToken);
  assert.deepEqual((await f.disk()).chats.alice.pendingAdmission, old.pendingAdmission);
  assert.equal(f.threadCreates, 0);
});

for (const limit of ["count", "bytes"]) test(`replacement account refuses ${limit} overflow without losing private receipts`, async (t) => {
  const f = await fixture(t);
  const retiredAccountStates = limit === "count"
    ? Object.fromEntries(Array.from({ length: 200 }, (_, i) => [`receipt-${i}`, { threadId: `old-${i}` }]))
    : { receipt: { prompt: "x".repeat(1024 * 1024) } };
  const old = { threadId: "old-thread", bindingAccountId: "bot-A", retiredAccountStates,
    pendingAdmission: { accountId: "bot-A", request: { prompt: "retained private prompt" } } };
  await fs.writeFile(path.join(f.dir, "thread-map.json"), JSON.stringify({ chats: { alice: old }, messages: [], inflight: {} }));
  await fs.writeFile(path.join(f.dir, "account.json"), JSON.stringify({ ...f.account, accountId: "bot-B" }));
  f.batches.push([incoming(1, "/new", "alice", "bot-B-context")]); f.start();
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text.includes("Retired account receipt storage is full")), "retirement storage limit was not reported");
  const current = (await f.disk()).chats.alice;
  assert.equal(f.threadCreates, 0);
  assert.equal(current.threadId, old.threadId);
  assert.equal(current.bindingAccountId, old.bindingAccountId);
  assert.deepEqual(current.retiredAccountStates, retiredAccountStates);
  assert.deepEqual(current.pendingAdmission, old.pendingAdmission);
  assert.equal(f.posts.length, 0);
  assert.equal(f.sent.some((msg) => msg.item_list[0].text_item.text.includes("retained private prompt")), false);
});

test("same-account new-thread command cannot discard a pending admission", async (t) => {
  const f = await fixture(t, { admission: () => {} }); f.batches.push([incoming(1, "pending")]); f.start();
  await until(() => f.posts.length === 1, "pending admission missing");
  f.batches.push([incoming(2, "/new")]);
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text.includes("New conversation failed: This chat still has a pending turn or retained reply")), "same-account pending state was overwritten or the control gave no answer");
  assert.equal(f.threadCreates, 1); assert.equal((await f.disk()).chats.alice.pendingAdmission.request.prompt, "pending");
});

test("resume and thread summaries retain account-and-chat ownership across new conversations", async (t) => {
  const f = await fixture(t, { summaries: [
    { id: "thread-1", title: "Alice private title" }, { id: "unbound-thread", title: "Other account private title" },
  ] });
  f.batches.push([incoming(1, "/new"), incoming(2, "/resume thread-1", "bob"), incoming(3, "/threads", "bob"), incoming(4, "/threads")]);
  f.start();
  await until(() => f.sent.length === 4, "ownership controls were not handled");
  const texts = f.sent.map((msg) => msg.item_list[0].text_item.text);
  assert.match(texts[1], /not owned by this account and chat/);
  assert.match(texts[2], /No recent runtime threads owned/);
  assert.match(texts[3], /Alice private title/); assert.ok(!texts.some((text) => text.includes("Other account private title")));
  assert.deepEqual(f.summaryQueries, [["thread-1"]], "the bridge requests only locally bound threads owned by this account and chat");
  assert.equal(f.threadReads.length, 0, "rejected resume must not read another chat's thread");
  assert.equal((await f.disk()).chats.bob.threadId, undefined);
  f.batches.push([incoming(5, "/new"), incoming(6, "/resume thread-1")]);
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text === "Resumed thread thread-1"), "owner could not resume its prior conversation");
  assert.equal((await f.disk()).chats.alice.threadId, "thread-1"); assert.equal(f.threadCreates, 2); assert.equal(f.threadReads.length, 1);
});

test("replacement account cannot resume the retired account's owned thread", async (t) => {
  const f = await fixture(t);
  await fs.writeFile(path.join(f.dir, "thread-map.json"), JSON.stringify({ chats: { alice: { threadId: "thread-1", bindingAccountId: "bot-A" } }, messages: [], inflight: {} }));
  await fs.writeFile(path.join(f.dir, "account.json"), JSON.stringify({ ...f.account, accountId: "bot-B" }));
  f.batches.push([incoming(1, "/resume thread-1")]); f.start();
  await until(() => f.sent.some((msg) => msg.item_list[0].text_item.text.includes("not owned by this account and chat")), "foreign account resumed a private conversation");
  assert.equal(f.threadReads.length, 0); assert.equal((await f.disk()).chats.alice.bindingAccountId, "bot-A");
});

test("restart between durable thread binding and admission update keeps the frozen prompt", async (t) => {
  const f = await fixture(t); const key = JSON.stringify(["bot-A", "alice", "1"]);
  const request = { prompt: "durable prompt", input_summary: "durable prompt", model: "frozen-model", mode: "agent", allow_shell: false, trust_mode: false, auto_approve: false, operation_key: "frozen-operation" };
  await fs.writeFile(path.join(f.dir, "thread-map.json"), JSON.stringify({ chats: { alice: {
    threadId: "thread-1", bindingAccountId: "bot-A", pendingAdmission: {
      accountId: "bot-A", actorId: "weixin-account:bot-A:user:alice", messageKey: key, request,
      operationKey: request.operation_key, threadId: null, submitted: false, sinceSeq: null,
    },
  } }, messages: [key], inflight: { [key]: "interrupted" } }));
  f.batches.push([incoming(1, "durable prompt")]); f.start();
  await until(() => f.streams.size, "durable thread binding lost its pending prompt on restart");
  assert.equal(f.threadCreates, 0); assert.equal(f.posts.length, 1); assert.deepEqual(f.posts[0], request);
  assert.equal((await f.disk()).chats.alice.turnDelivery.messageKey, key);
});
