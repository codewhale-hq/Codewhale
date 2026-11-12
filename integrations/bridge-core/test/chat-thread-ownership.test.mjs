import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { ThreadStore, THREAD_NOT_OWNED_MESSAGE } from "../src/lib.mjs";
import { startStub } from "./support/stub-server.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const integrations = path.join(here, "..", "..");
const NOT_OWNED = new RegExp(THREAD_NOT_OWNED_MESSAGE.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
const NOTE = "ALICE_PRIVATE_NOTE";

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
  if (seedMap) await writeFile(mapPath, JSON.stringify(seedMap));
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
        CODEWHALE_WORKSPACE: "/fixture/bridge-workspace",
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
    }, { seedMap, seedThreads: [{ id: "thr-legacy", workspace: "/fixture/bridge-workspace", summary: "legacy" }] });
  });
}

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

test("an existing store migrates once: unique threads are owned, shared ones are owned by no chat", async () => {
  const seed = { chats: {
    a: { threadId: "only-a" }, b: { threadId: "shared" }, c: { threadId: "shared" },
    w: { threadId: "w-now", bindingAccountId: "acct", threadBindings: [{ threadId: "w-old", accountId: "acct", actorId: "weixin-account:acct:user:w" }] }
  } };
  const legacyOwnerScope = (chatId, state) => (state.bindingAccountId ? `weixin-account:${state.bindingAccountId}:user:${chatId}` : "");
  await withStore(async (store, file) => {
    assert.ok(store.ownsThread("a", "only-a"));
    assert.ok(!store.ownsThread("b", "shared") && !store.ownsThread("c", "shared"));
    const scope = "weixin-account:acct:user:w";
    assert.ok(store.ownsThread("w", "w-now", scope) && store.ownsThread("w", "w-old", scope));
    assert.ok(!store.ownsThread("w", "w-now"));
    await store.recordThreadOwner("a", "fresh");
    await store.setChat("c", { threadId: "only-a" });
    const reopened = await ThreadStore.open(file, { legacyOwnerScope });
    assert.ok(!reopened.ownsThread("c", "only-a"), "the migration does not run again once owners are recorded");
    assert.ok(reopened.ownsThread("a", "only-a"));
  }, { legacyOwnerScope }, seed);
});
