import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { ThreadStore, THREAD_NOT_OWNED_MESSAGE, decideApproval } from "../src/lib.mjs";
import { startStub } from "./support/stub-server.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const integrations = path.join(here, "..", "..");
const NOT_OWNED = new RegExp(THREAD_NOT_OWNED_MESSAGE.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
const NOTE = "ALICE_PRIVATE_NOTE";
const PROMPT_TAIL = "Keep status updates concise. Ask for tool approvals when needed; do not assume mobile messages imply blanket approval.";
const BRIDGE_WORKSPACE = "/fixture/bridge-workspace";

async function until(check, label, timeoutMs = 30000) {
  const started = Date.now();
  for (;;) {
    const value = check();
    if (value) return value;
    if (Date.now() - started > timeoutMs) assert.fail(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

async function withBridge(name, env, run, { seedMap = null, seedThreads = [], prepare = null } = {}) {
  const stub = await startStub();
  for (const thread of seedThreads) stub.seedThread(thread);
  if (prepare) prepare(stub);
  const scratch = await mkdtemp(path.join(tmpdir(), "cw-bridge-ownership-"));
  const mapPath = path.join(scratch, "thread-map.json");
  const seeded = typeof seedMap === "function" ? seedMap(stub) : seedMap;
  if (seeded) await writeFile(mapPath, JSON.stringify(seeded));
  const logs = [];
  const child = spawn(
    process.execPath,
    ["--import", pathToFileURL(path.join(here, "support/register.mjs")).href, path.join(integrations, name, "src/index.mjs")],
    {
      env: {
        PATH: process.env.PATH,
        FAKE_CHAT_URL: stub.url,
        CODEWHALE_RUNTIME_URL: stub.url,
        CODEWHALE_RUNTIME_TOKEN: "fixture-runtime-token",
        CODEWHALE_WORKSPACE: BRIDGE_WORKSPACE,
        ...env(stub, mapPath)
      },
      stdio: ["ignore", "pipe", "pipe"]
    }
  );
  child.stdout.on("data", (chunk) => logs.push(String(chunk)));
  child.stderr.on("data", (chunk) => logs.push(String(chunk)));
  const repliesTo = (who) => stub.state.replies.filter((reply) => reply.to === who).map((reply) => reply.text);
  const say = async (push, who, text, expect) => {
    const before = repliesTo(who).length;
    push(who, text);
    return until(() => {
      const fresh = repliesTo(who).slice(before);
      return fresh.some((reply) => expect.test(reply)) ? fresh : null;
    }, `${name}: reply to ${who} for ${JSON.stringify(text)} matching ${expect}`).catch((error) => {
      console.log(`replies=${JSON.stringify(stub.state.replies)} logs=${logs.join("")}`);
      throw error;
    });
  };
  try {
    await run({ stub, say, mapPath });
  } finally {
    child.kill("SIGKILL");
    await stub.close();
    await rm(scratch, { recursive: true, force: true });
  }
}

const bridges = {
  wecom: {
    name: "wecom-bridge",
    systemPrompt: `You are being controlled from a WeCom (企业微信) phone chat. ${PROMPT_TAIL}`,
    alice: "alice", bob: "bob",
    chatKey: (who) => `single:${who}`,
    env: (stub, mapPath) => ({
      WECOM_BOT_ID: "fixture-bot", WECOM_BOT_SECRET: "fixture-not-a-secret",
      WECOM_CHAT_ALLOWLIST: "alice,bob", WECOM_THREAD_MAP_PATH: mapPath
    }),
    push: (stub) => (who, text) => stub.pushWecom(who, text)
  },
  feishu: {
    name: "feishu-bridge",
    systemPrompt: `You are being controlled from a Feishu/Lark phone chat. ${PROMPT_TAIL}`,
    alice: "oc_alice", bob: "oc_bob",
    chatKey: (who) => who,
    env: (stub, mapPath) => ({
      FEISHU_APP_ID: "fixture-app", FEISHU_APP_SECRET: "fixture-not-a-secret",
      CODEWHALE_CHAT_ALLOWLIST: "oc_alice,oc_bob", FEISHU_THREAD_MAP_PATH: mapPath
    }),
    push: (stub) => (who, text) => stub.pushFeishu(`ou_${who}`, who, text)
  },
  telegram: {
    name: "telegram-bridge",
    systemPrompt: `You are being controlled from a Telegram phone chat. ${PROMPT_TAIL}`,
    alice: "111", bob: "222",
    chatKey: (who) => who,
    env: (stub, mapPath) => ({
      TELEGRAM_BOT_TOKEN: "1000:fixture-not-a-token", TELEGRAM_API_BASE: `${stub.url}/tg`,
      TELEGRAM_CHAT_ALLOWLIST: "111,222", TELEGRAM_THREAD_MAP_PATH: mapPath, TELEGRAM_POLL_TIMEOUT_SECONDS: "1"
    }),
    push: (stub) => (who, text) => stub.pushTelegram(Number(who), text)
  }
};

const turnPosts = (stub, threadId) => stub.state.runtimeRequests.filter((request) => request.route === `POST /v1/threads/${threadId}/turns`);

for (const [label, bridge] of Object.entries(bridges)) {
  test(`${label}: a chat lists and resumes only the threads it started`, async () => {
    await withBridge(bridge.name, bridge.env, async ({ stub, say, mapPath }) => {
      const push = bridge.push(stub);
      const { alice, bob } = bridge;

      await say(push, alice, "/new", /Created thread/);
      const aliceThread = stub.state.threads[0].id;
      await say(push, alice, `remember ${NOTE}`, /STUB-MODEL/);

      const bobListing = (await say(push, bob, "/threads", /No threads started|thr-/)).join("\n");
      assert.ok(!bobListing.includes(aliceThread) && !bobListing.includes(NOTE), "another chat's thread is not listed");

      const refused = (await say(push, bob, `/resume ${aliceThread}`, /Resumed|can only use threads/)).join("\n");
      assert.match(refused, NOT_OWNED);
      const unknown = (await say(push, bob, "/resume thr-does-not-exist", /Resumed|can only use threads|failed/i)).join("\n");
      assert.match(unknown, NOT_OWNED, "an unknown id gets the same reply as a foreign one");
      assert.ok(!stub.state.runtimeRequests.some((request) => request.route === "GET /v1/threads/thr-does-not-exist"),
        "an unowned id never reaches the Runtime");

      const map = JSON.parse(await readFile(mapPath, "utf8"));
      assert.notEqual(map.chats[bridge.chatKey(bob)]?.threadId, aliceThread, "the refusal leaves the chat unbound");

      await say(push, bob, "hello from bob", /STUB-MODEL/);
      assert.equal(turnPosts(stub, aliceThread).filter((request) => request.body?.prompt === "hello from bob").length, 0);

      const aliceListing = (await say(push, alice, "/threads", /thr-/)).join("\n");
      assert.ok(aliceListing.includes(aliceThread) && aliceListing.includes(NOTE));
      const created = (await say(push, alice, "/new", /Created thread/)).join("\n");
      const secondThread = created.match(/Created thread (\S+)/)[1];
      assert.notEqual(secondThread, aliceThread, "/new starts a thread even when the chat already has one");
      assert.ok(stub.state.threads.some((thread) => thread.id === secondThread), "the new thread exists in the Runtime");
      await say(push, alice, `/resume ${aliceThread}`, /Resumed thread/);
      await say(push, alice, "still mine", /STUB-MODEL/);
      assert.equal(turnPosts(stub, aliceThread).filter((request) => request.body?.prompt === "still mine").length, 1);
      assert.notEqual(secondThread, aliceThread);
    });
  });

  test(`${label}: a thread no chat of this bridge started is neither listed nor resumable`, async () => {
    await withBridge(bridge.name, bridge.env, async ({ stub, say }) => {
      const push = bridge.push(stub);
      const { bob } = bridge;
      const listing = (await say(push, bob, "/threads", /No threads started|thr-/)).join("\n");
      assert.ok(!listing.includes("thr-desktop"));
      const refused = (await say(push, bob, "/resume thr-desktop", /Resumed|can only use threads/)).join("\n");
      assert.match(refused, NOT_OWNED);
      assert.ok(!stub.state.runtimeRequests.some((request) => request.route.includes("/v1/threads/thr-desktop")));
    }, { seedThreads: [{ id: "thr-desktop", workspace: "/fixture/other-project", summary: `desktop work ${NOTE}`, runningTurnId: "turn-desktop" }] });
  });

  test(`${label}: a thread shared by two chats in an existing thread map fails closed`, async () => {
    const seedMap = { chats: {
      [bridge.chatKey(bridge.alice)]: { threadId: "thr-shared", lastSeq: 0, activeTurnId: null },
      [bridge.chatKey(bridge.bob)]: { threadId: "thr-shared", lastSeq: 0, activeTurnId: null }
    } };
    await withBridge(bridge.name, bridge.env, async ({ stub, say }) => {
      const push = bridge.push(stub);
      const reply = (await say(push, bridge.bob, "continue please", /can only use threads|STUB-MODEL/)).join("\n");
      assert.match(reply, NOT_OWNED);
      assert.equal(turnPosts(stub, "thr-shared").length, 0, "no turn reaches the shared thread");
      const listing = (await say(push, bridge.alice, "/threads", /No threads started|thr-/)).join("\n");
      assert.ok(!listing.includes("thr-shared"));
      const created = (await say(push, bridge.bob, "/new", /Created thread|can only use threads/)).join("\n");
      assert.doesNotMatch(created, NOT_OWNED, "/new gives the chat a thread of its own");
      const fresh = created.match(/Created thread (\S+)/)[1];
      assert.notEqual(fresh, "thr-shared");
      await say(push, bridge.bob, "hello after new", /STUB-MODEL/);
      assert.equal(turnPosts(stub, fresh).length, 1);
      assert.equal(turnPosts(stub, "thr-shared").length, 0);
    }, { seedMap, seedThreads: [{ id: "thr-shared", workspace: "/fixture/bridge-workspace", summary: "shared" }] });
  });

  test(`${label}: a thread only one chat points at in an existing thread map stays with that chat`, async () => {
    const seedMap = { chats: { [bridge.chatKey(bridge.alice)]: { threadId: "thr-legacy", lastSeq: 0, activeTurnId: null } } };
    await withBridge(bridge.name, bridge.env, async ({ stub, say }) => {
      const push = bridge.push(stub);
      await say(push, bridge.alice, "carry on", /STUB-MODEL/);
      assert.equal(turnPosts(stub, "thr-legacy").length, 1);
      const refused = (await say(push, bridge.bob, "/resume thr-legacy", /Resumed|can only use threads/)).join("\n");
      assert.match(refused, NOT_OWNED);
    }, { seedMap, seedThreads: [{ id: "thr-legacy", workspace: BRIDGE_WORKSPACE, systemPrompt: bridge.systemPrompt, summary: "legacy" }] });
  });

  test(`${label}: a chat bound to a thread started outside the bridge at upgrade does not keep it`, async () => {
    const seedMap = { chats: { [bridge.chatKey(bridge.bob)]: { threadId: "thr-desktop", lastSeq: 0, activeTurnId: null } } };
    await withBridge(bridge.name, bridge.env, async ({ stub, say, mapPath }) => {
      const push = bridge.push(stub);
      const listing = (await say(push, bridge.bob, "/threads", /No threads started|thr-/)).join("\n");
      assert.ok(!listing.includes("thr-desktop") && !listing.includes(NOTE), "the thread is not listed");
      const reply = (await say(push, bridge.bob, "what was I doing?", /can only use threads|STUB-MODEL/)).join("\n");
      assert.match(reply, NOT_OWNED);
      assert.equal(turnPosts(stub, "thr-desktop").length, 0, "no turn reaches the thread");
      const refused = (await say(push, bridge.bob, "/resume thr-desktop", /Resumed|can only use threads/)).join("\n");
      assert.match(refused, NOT_OWNED);
      const map = JSON.parse(await readFile(mapPath, "utf8"));
      assert.ok(!map.threadOwners.some((entry) => entry.threadId === "thr-desktop"), "no ownership is recorded for it");
    }, { seedMap, seedThreads: [{ id: "thr-desktop", workspace: "/fixture/other-project", summary: `desktop work ${NOTE}` }] });
  });

  test(`${label}: a bridge thread kept at upgrade is confirmed once and stays owned after a restart`, async () => {
    const seedMap = { chats: { [bridge.chatKey(bridge.alice)]: { threadId: "thr-legacy", lastSeq: 0, activeTurnId: null } } };
    await withBridge(bridge.name, bridge.env, async ({ stub, say, mapPath }) => {
      const push = bridge.push(stub);
      await say(push, bridge.alice, "carry on", /STUB-MODEL/);
      const map = JSON.parse(await readFile(mapPath, "utf8"));
      const entry = map.threadOwners.find((owner) => owner.threadId === "thr-legacy");
      assert.ok(entry && entry.legacy === undefined, "the grant is confirmed and no longer marked");
    }, { seedMap, seedThreads: [{ id: "thr-legacy", workspace: BRIDGE_WORKSPACE, systemPrompt: bridge.systemPrompt, summary: "legacy" }] });
  });

  test(`${label}: a chat on a disowned shared thread cannot decide an approval on it`, async () => {
    const bobKey = bridge.chatKey(bridge.bob);
    const actor = { wecom: "wecom-user:bob", feishu: "feishu-openId:ou_oc_bob", telegram: "telegram-user:222" }[label];
    const seedMap = { chats: {
      [bridge.chatKey(bridge.alice)]: { threadId: "thr-shared", lastSeq: 0, activeTurnId: null },
      [bobKey]: { threadId: "thr-shared", lastSeq: 0, activeTurnId: null,
        turnOrigins: [{ threadId: "thr-shared", turnId: "turn-bob", actorId: actor }],
        authorizedIdentity: { chatId: bobKey, chatType: label === "feishu" ? "p2p" : "private", userId: bridge.bob, openId: "ou_oc_bob", isBot: false } }
    } };
    await withBridge(bridge.name, bridge.env, async ({ stub, say }) => {
      const push = bridge.push(stub);
      const reply = (await say(push, bridge.bob, "/allow appr-1", /Approval|can only use threads|not waiting/)).join("\n");
      assert.match(reply, NOT_OWNED);
      assert.equal(stub.state.approvalDecisions.length, 0, "the decision never reaches the Runtime");
    }, { seedMap, seedThreads: [{ id: "thr-shared", workspace: BRIDGE_WORKSPACE, systemPrompt: bridge.systemPrompt, summary: "shared", runningTurnId: "turn-bob",
      pendingApprovals: [{ id: "appr-1", turn_id: "turn-bob" }] }] });
  });

  test(`${label}: a chat on its own thread can still decide an approval`, async () => {
    const aliceKey = bridge.chatKey(bridge.alice);
    const actor = { wecom: "wecom-user:alice", feishu: "feishu-openId:ou_oc_alice", telegram: "telegram-user:111" }[label];
    const seedMap = { chats: { [aliceKey]: { threadId: "thr-own", lastSeq: 0, activeTurnId: null,
      turnOrigins: [{ threadId: "thr-own", turnId: "turn-alice", actorId: actor }],
      authorizedIdentity: { chatId: aliceKey, chatType: label === "feishu" ? "p2p" : "private", userId: bridge.alice, openId: "ou_oc_alice", isBot: false } } } };
    await withBridge(bridge.name, bridge.env, async ({ stub, say }) => {
      const push = bridge.push(stub);
      await say(push, bridge.alice, "/allow appr-1", /Approval appr-1: allow/);
      assert.deepEqual(stub.state.approvalDecisions.map((entry) => entry.id), ["appr-1"]);
    }, { seedMap, seedThreads: [{ id: "thr-own", workspace: BRIDGE_WORKSPACE, systemPrompt: bridge.systemPrompt, summary: "own", runningTurnId: "turn-alice",
      pendingApprovals: [{ id: "appr-1", turn_id: "turn-alice" }] }] });
  });
}

test("wecom: a chat bound to a thread started outside the bridge cannot steer that thread's running turn", async () => {
  const bridge = bridges.wecom;
  const seedMap = { chats: { "single:bob": { threadId: "thr-desktop", lastSeq: 0, activeTurnId: null } } };
  await withBridge(bridge.name, bridge.env, async ({ stub, say }) => {
    const push = bridge.push(stub);
    const reply = (await say(push, "bob", "rm the tests and push", /can only use threads|forwarded|active turn/)).join("\n");
    assert.match(reply, NOT_OWNED);
    assert.ok(!stub.state.runtimeRequests.some((request) => /\/steer$/.test(request.route)), "no steer reaches the thread");
  }, { seedMap, seedThreads: [{ id: "thr-desktop", workspace: "/fixture/other-project", summary: `desktop work ${NOTE}`, runningTurnId: "turn-desktop" }] });
});

test("feishu: a chat bound to a thread started outside the bridge does not receive its running turn on restart", async () => {
  const bridge = bridges.feishu;
  const seedMap = { chats: { oc_bob: { threadId: "thr-desktop", lastSeq: 0, activeTurnId: "turn-old",
    authorizedIdentity: { chatId: "oc_bob", chatType: "p2p", openId: "ou_oc_bob" } } } };
  await withBridge(bridge.name, bridge.env, async ({ stub, say, mapPath }) => {
    const push = bridge.push(stub);
    await say(push, "oc_bob", "/help", /./);
    await new Promise((resolve) => setTimeout(resolve, 1500));
    const toBob = stub.state.replies.filter((reply) => reply.to === "oc_bob").map((reply) => reply.text).join("\n");
    assert.ok(!toBob.includes("OTHER_CLIENT_OUTPUT") && !/Reattaching/.test(toBob), "no output from the thread is relayed");
    assert.ok(!stub.state.runtimeRequests.some((request) => request.route.endsWith("/events")), "the thread's events are never read");
    const map = JSON.parse(await readFile(mapPath, "utf8"));
    assert.equal(map.chats.oc_bob.activeTurnId, null, "the stored turn id is cleared");
  }, { seedMap, prepare: (stub) => {
    const entry = stub.seedThread({ id: "thr-desktop", workspace: "/fixture/other-project", summary: "desktop work", runningTurnId: "turn-desktop" });
    entry.events.push({ seq: 1, thread_id: "thr-desktop", turn_id: "turn-desktop", event: "item.delta", payload: { kind: "agent_message", delta: "OTHER_CLIENT_OUTPUT" } });
    entry.events.push({ seq: 2, thread_id: "thr-desktop", turn_id: "turn-desktop", event: "turn.completed", payload: { turn: { status: "completed" } } });
  } });
});

const otherChatEvents = (entry) => {
  entry.events.push({ seq: 1, thread_id: "thr-shared", turn_id: "turn-other", event: "item.delta", payload: { kind: "agent_message", delta: "OTHER_CHAT_OUTPUT" } });
  entry.events.push({ seq: 2, thread_id: "thr-shared", turn_id: "turn-other", event: "turn.completed", payload: { turn: { status: "completed" } } });
};

test("feishu: a chat the migration disowned does not reattach to another chat's running turn on restart", async () => {
  const bridge = bridges.feishu;
  const seedMap = { chats: {
    oc_alice: { threadId: "thr-shared", lastSeq: 0, activeTurnId: null },
    oc_bob: { threadId: "thr-shared", lastSeq: 0, activeTurnId: "turn-stale",
      authorizedIdentity: { chatId: "oc_bob", chatType: "p2p", openId: "ou_oc_bob" } }
  } };
  await withBridge(bridge.name, bridge.env, async ({ stub, say, mapPath }) => {
    const push = bridge.push(stub);
    const listing = (await say(push, "oc_bob", "/threads", /No threads started|thr-/)).join("\n");
    assert.ok(!listing.includes("thr-shared"));
    await new Promise((resolve) => setTimeout(resolve, 500));
    const toBob = stub.state.replies.filter((reply) => reply.to === "oc_bob").map((reply) => reply.text).join("\n");
    assert.ok(!toBob.includes("OTHER_CHAT_OUTPUT") && !/Reattaching/.test(toBob), "no other chat's turn output is relayed");
    assert.ok(!stub.state.runtimeRequests.some((request) => request.route.includes("/v1/threads/thr-shared")), "the shared thread is never read");
    const map = JSON.parse(await readFile(mapPath, "utf8"));
    assert.equal(map.chats.oc_bob.activeTurnId, null, "the stale turn id is cleared");
  }, { seedMap, prepare: (stub) => otherChatEvents(stub.seedThread({ id: "thr-shared", workspace: "/fixture/bridge-workspace", summary: "shared", runningTurnId: "turn-other" })) });
});

test("telegram: a chat the migration disowned does not stream another chat's turn and can start over", async () => {
  const bridge = bridges.telegram;
  const seedMap = { chats: {
    "111": { threadId: "thr-shared", lastSeq: 0, activeTurnId: null },
    "222": { threadId: "thr-shared", lastSeq: 0, activeTurnId: "turn-other", activeMessageKey: "stale-key",
      authorizedIdentity: { chatId: "222", chatType: "private", userId: "222", isBot: false } }
  } };
  await withBridge(bridge.name, bridge.env, async ({ stub, say, mapPath }) => {
    const push = bridge.push(stub);
    const created = (await say(push, "222", "/new", /Created thread|pending turn|can only use threads/)).join("\n");
    assert.match(created, /Created thread/, "the stale turn does not block /new");
    const toBob = stub.state.replies.filter((reply) => reply.to === "222").map((reply) => reply.text).join("\n");
    assert.ok(!toBob.includes("OTHER_CHAT_OUTPUT"), "no other chat's turn output is relayed");
    assert.ok(!stub.state.runtimeRequests.some((request) => request.route === "GET /v1/threads/thr-shared/events"), "the shared thread's events are never read");
    const map = JSON.parse(await readFile(mapPath, "utf8"));
    assert.equal(map.chats["222"].activeTurnId, null);
  }, { seedMap, prepare: (stub) => otherChatEvents(stub.seedThread({ id: "thr-shared", workspace: "/fixture/bridge-workspace", summary: "shared", runningTurnId: "turn-other" })) });
});

async function withStore(fn, options = {}, seed = null) {
  const dir = await mkdtemp(path.join(tmpdir(), "cw-thread-owners-"));
  const file = path.join(dir, "map.json");
  try {
    if (seed) await writeFile(file, JSON.stringify(seed));
    await fn(await ThreadStore.open(file, options), file);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

test("thread owners are durable, survive a chat state replacement and never transfer", async () => {
  await withStore(async (store, file) => {
    await store.recordThreadOwner("a", "t1");
    await store.setChat("a", { threadId: "t1" });
    await store.setChat("a", { threadId: "t2" });
    assert.ok(store.ownsThread("a", "t1"));
    assert.ok(!store.ownsThread("b", "t1"));
    assert.ok(!store.ownsThread("a", "t1", "other-scope"));
    await assert.rejects(store.recordThreadOwner("b", "t1"), /can only use threads/);
    await store.recordThreadOwner("a", "t1");
    const reopened = await ThreadStore.open(file);
    assert.ok(reopened.ownsThread("a", "t1"));
    assert.deepEqual(reopened.ownedThreadIds("a"), ["t1"]);
  });
});

test("thread owners are bounded per chat and drop the oldest first", async () => {
  await withStore(async (store) => {
    for (const id of ["t1", "t2", "t3"]) await store.recordThreadOwner("a", id);
    await store.recordThreadOwner("b", "u1");
    assert.deepEqual(store.ownedThreadIds("a"), ["t3", "t2"]);
    assert.ok(store.ownsThread("b", "u1"));
  }, { ownerLimit: 2 });
});

function runtimeOf(threads, { failing = new Set() } = {}) {
  const calls = [];
  const runtimeJson = async (route) => {
    calls.push(route);
    const id = decodeURIComponent(route.split("/").at(-1));
    if (failing.has(id)) throw Object.assign(new Error("connection refused"), { status: undefined });
    if (!(id in threads)) throw Object.assign(new Error("not found"), { status: 404 });
    return { thread: threads[id] };
  };
  return { runtimeJson, calls };
}

test("an existing store migrates once: unique threads wait for confirmation, shared ones are owned by no chat", async () => {
  const seed = { chats: {
    a: { threadId: "only-a" }, b: { threadId: "shared" }, c: { threadId: "shared" },
    w: { threadId: "w-now", bindingAccountId: "acct", threadBindings: [{ threadId: "w-old", accountId: "acct", actorId: "weixin-account:acct:user:w" }] }
  } };
  const legacyOwnerScope = (chatId, state) => (state.bindingAccountId ? `weixin-account:${state.bindingAccountId}:user:${chatId}` : "");
  const confirmed = { system_prompt: "bridge", workspace: "/w" };
  const accept = (thread) => thread.system_prompt === "bridge";
  await withStore(async (store, file) => {
    assert.ok(!store.ownsThread("a", "only-a"), "a migrated grant is not usable before it is confirmed");
    assert.ok(store.ownershipPending("a", "only-a"), "an unconfirmed grant is pending");
    assert.deepEqual(store.ownedThreadIds("a"), []);
    const scope = "weixin-account:acct:user:w";
    assert.ok(store.ownsThread("w", "w-old", scope), "an old per-account binding was recorded by the bridge itself and needs no confirmation");
    assert.ok(!store.ownershipPending("w", "w-old", scope));
    const { runtimeJson, calls } = runtimeOf({ "only-a": confirmed, "w-now": confirmed, shared: confirmed });
    assert.equal(await store.verifyLegacyOwners(runtimeJson, accept), 0);
    assert.deepEqual(calls.sort(), ["/v1/threads/only-a", "/v1/threads/w-now"], "only unshared migrated grants are looked up, once each");
    assert.ok(store.ownsThread("a", "only-a"));
    assert.ok(!store.ownershipPending("a", "only-a"));
    assert.ok(!store.ownsThread("b", "shared") && !store.ownsThread("c", "shared"));
    assert.ok(store.ownsThread("w", "w-now", scope) && !store.ownsThread("w", "w-now"));
    await store.recordThreadOwner("a", "fresh");
    await store.setChat("c", { threadId: "only-a" });
    const reopened = await ThreadStore.open(file, { legacyOwnerScope });
    assert.ok(!reopened.ownsThread("c", "only-a"), "the migration does not run again once owners are recorded");
    assert.ok(reopened.ownsThread("a", "only-a"), "a confirmed grant survives a restart");
    assert.equal(reopened.legacyOwnerCount(), 0);
  }, { legacyOwnerScope }, seed);
});

test("a migrated grant is dropped when the thread was not started by the bridge or is gone, and kept unconfirmed when the Runtime cannot be asked", async () => {
  const seed = { chats: { a: { threadId: "t-ok" }, b: { threadId: "t-foreign" }, c: { threadId: "t-gone" }, d: { threadId: "t-down" } } };
  const accept = (thread) => thread.system_prompt === "bridge" && thread.workspace === "/w";
  await withStore(async (store, file) => {
    const threads = {
      "t-ok": { system_prompt: "bridge", workspace: "/w" },
      "t-foreign": { workspace: "/w" },
      "t-down": { system_prompt: "bridge", workspace: "/w" }
    };
    assert.equal(await store.verifyLegacyOwners(runtimeOf(threads, { failing: new Set(["t-down"]) }).runtimeJson, accept), 1);
    assert.ok(store.ownsThread("a", "t-ok"));
    assert.ok(!store.ownershipPending("a", "t-ok"));
    for (const [chat, id] of [["b", "t-foreign"], ["c", "t-gone"], ["d", "t-down"]]) assert.ok(!store.ownsThread(chat, id), id);
    assert.ok(!store.ownershipPending("b", "t-foreign") && !store.ownershipPending("c", "t-gone"));
    assert.ok(store.ownershipPending("d", "t-down"));
    const reopened = await ThreadStore.open(file);
    assert.equal(reopened.legacyOwnerCount(), 1, "the unconfirmed grant is still pending after a restart");
    assert.ok(reopened.ownershipPending("d", "t-down"));
    assert.ok(!reopened.data.threadOwners.some((entry) => entry.threadId === "t-foreign" || entry.threadId === "t-gone"));
    assert.equal(await reopened.verifyLegacyOwners(runtimeOf(threads).runtimeJson, accept), 0);
    assert.ok(reopened.ownsThread("d", "t-down"), "a later check confirms it");
    assert.ok(!reopened.ownershipPending("d", "t-down"));
  }, {}, seed);
});

test("pending-ownership recovery preserves stored turns and retained admissions when Runtime thread reads return 503 at startup", async () => {
  const tg = bridges.telegram;
  const fsBridge = bridges.feishu;
  await Promise.all([
    withBridge(tg.name, tg.env, async ({ stub }) => {
      await until(
        () => turnPosts(stub, "thr-own").some((request) => request.body?.prompt === "RETAINED_PROMPT"),
        "retained admission should post after ownership confirms",
        45000
      );
    }, {
      seedThreads: [{ id: "thr-own", workspace: BRIDGE_WORKSPACE, systemPrompt: tg.systemPrompt, summary: "own" }],
      prepare: (stub) => { stub.state.failDetailUntil = Date.now() + 4000; },
      seedMap: (stub) => ({ chats: { "222": {
        threadId: "thr-own", lastSeq: 0, activeTurnId: null,
        authorizedIdentity: { chatId: "222", chatType: "private", userId: "222", isBot: false },
        pendingAdmission: {
          botAccountId: "1000", runtimeUrl: stub.url, actorId: "telegram-user:222",
          identity: { chatId: "222", chatType: "private", userId: "222", username: "", isBot: false },
          messageKey: "telegram:222:77", request: { prompt: "RETAINED_PROMPT", input_summary: "RETAINED_PROMPT" },
          operationKey: null, threadId: "thr-own", submitted: false, sinceSeq: null,
          threadRequest: { workspace: BRIDGE_WORKSPACE, system_prompt: tg.systemPrompt }
        }
      } } })
    }),
    withBridge(tg.name, tg.env, async ({ stub }) => {
      await until(
        () => stub.state.replies.some((reply) => reply.to === "111" && reply.text.includes("OWN_TURN_OUTPUT")),
        "telegram stored turn should relay after ownership confirms",
        45000
      );
    }, {
      seedMap: { chats: { "111": {
        threadId: "thr-own", lastSeq: 0, activeTurnId: "turn-own", activeMessageKey: "k1",
        authorizedIdentity: { chatId: "111", chatType: "private", userId: "111", isBot: false }
      } } },
      prepare: (stub) => {
        stub.state.failDetailUntil = Date.now() + 4000;
        const entry = stub.seedThread({ id: "thr-own", workspace: BRIDGE_WORKSPACE, systemPrompt: tg.systemPrompt, summary: "own", runningTurnId: "turn-own" });
        entry.events.push({ seq: 1, thread_id: "thr-own", turn_id: "turn-own", event: "item.delta", payload: { kind: "agent_message", delta: "OWN_TURN_OUTPUT" } });
        entry.events.push({ seq: 2, thread_id: "thr-own", turn_id: "turn-own", event: "turn.completed", payload: { turn: { status: "completed" } } });
      }
    }),
    withBridge(fsBridge.name, fsBridge.env, async ({ stub }) => {
      await until(
        () => stub.state.replies.some((reply) => reply.to === "oc_alice" && reply.text.includes("OWN_FEISHU_OUTPUT")),
        "feishu stored turn should reattach after ownership confirms",
        45000
      );
    }, {
      seedMap: { chats: { oc_alice: {
        threadId: "thr-own", lastSeq: 0, activeTurnId: "turn-own",
        authorizedIdentity: { chatId: "oc_alice", chatType: "p2p", openId: "ou_oc_alice" }
      } } },
      prepare: (stub) => {
        stub.state.failDetailUntil = Date.now() + 4000;
        const entry = stub.seedThread({ id: "thr-own", workspace: BRIDGE_WORKSPACE, systemPrompt: fsBridge.systemPrompt, summary: "own", runningTurnId: "turn-own" });
        entry.events.push({ seq: 1, thread_id: "thr-own", turn_id: "turn-own", event: "item.delta", payload: { kind: "agent_message", delta: "OWN_FEISHU_OUTPUT" } });
        entry.events.push({ seq: 2, thread_id: "thr-own", turn_id: "turn-own", event: "turn.completed", payload: { turn: { status: "completed" } } });
      }
    })
  ]);
});

test("core approval decisions check ownership only when the caller supplies it", async () => {
  await withStore(async (store) => {
    await store.setChat("a", { threadId: "t1" });
    await store.recordTurnOrigin("a", "t1", "turn-1", "human-1");
    const posts = [];
    const runtimeJson = async (route, options = {}) => {
      if (options.method === "POST") { posts.push(route); return {}; }
      return { thread: {}, pending_approvals: [{ id: "appr-1", turn_id: "turn-1" }] };
    };
    const args = { store, chatId: "a", actorId: "human-1", approvalId: "appr-1", decision: "allow" };
    await assert.rejects(decideApproval(runtimeJson, { ...args, ownsThread: (id) => store.ownsThread("a", id) }), /can only use threads/);
    assert.deepEqual(posts, []);
    await decideApproval(runtimeJson, args);
    assert.deepEqual(posts, ["/v1/approvals/appr-1"], "a store that records no owners keeps its behaviour");
  });
});
