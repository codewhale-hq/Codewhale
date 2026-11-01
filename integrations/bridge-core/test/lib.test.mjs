import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";

import {
  activeTurnBlock,
  commandAction,
  createRuntimeClient,
  envFirst,
  parseBool,
  parseCommand,
  parseEnvText,
  parseList,
  parseTextContent,
  preservedChatStateFields,
  readJsonSafe,
  readSse,
  splitMessage,
  stripGroupPrefix,
  ThreadStore,
  writeFileDurable
} from "../src/lib.mjs";

test("env and primitive parsers handle bridge env conventions", () => {
  assert.equal(envFirst({ A: "", B: " value " }, "A", "B"), "value");
  assert.deepEqual(parseList(" a, b ,, "), ["a", "b"]);
  assert.equal(parseBool("yes"), true);
  assert.equal(parseBool("0", true), false);
  assert.deepEqual(parseEnvText("export A='one'\nB=\"two\"\n# nope"), { A: "one", B: "two" });
  assert.deepEqual(parseEnvText("A='\nB=\"\nEMPTY=\"\""), { A: "'", B: '"', EMPTY: "" });
});

test("parseTextContent supports plain text and JSON text/content wrappers", () => {
  assert.equal(parseTextContent("hello"), "hello");
  assert.equal(parseTextContent(JSON.stringify({ text: "hello" })), "hello");
  assert.equal(parseTextContent(JSON.stringify({ content: "hello" })), "hello");
});

test("stripGroupPrefix supports direct chat types and prefixed group text", () => {
  assert.deepEqual(
    stripGroupPrefix("inspect", {
      chatType: "private",
      requirePrefix: true,
      prefix: "/cw",
      directChatTypes: ["private"]
    }),
    { accepted: true, text: "inspect" }
  );
  assert.deepEqual(
    stripGroupPrefix("/cw inspect", {
      chatType: "group",
      requirePrefix: true,
      prefix: "/cw",
      directChatTypes: ["private"]
    }),
    { accepted: true, text: "inspect" }
  );
});

test("commands map common actions while menu/start stay opt in", () => {
  assert.deepEqual(parseCommand("/allow@CodeWhaleBot ap_1 remember", { stripBotMention: true }), {
    name: "allow",
    args: "ap_1 remember"
  });
  assert.deepEqual(parseCommand("/allow@CodeWhaleBot ap_1 remember"), {
    name: "allow@codewhalebot",
    args: "ap_1 remember"
  });
  assert.deepEqual(commandAction(parseCommand("/status")), { kind: "status" });
  assert.deepEqual(commandAction(parseCommand("/menu")), { kind: "prompt", prompt: "/menu" });
  assert.deepEqual(commandAction(parseCommand("/menu"), { allowMenu: true }), { kind: "menu" });
  assert.deepEqual(commandAction(parseCommand("/start"), { allowStart: true }), { kind: "help" });
});

test("state/message/runtime helpers preserve bridge behavior", () => {
  assert.deepEqual(
    preservedChatStateFields({ model: "m", replyToMessageId: "r", ignored: true }, [
      "model",
      "replyToMessageId"
    ]),
    { model: "m", replyToMessageId: "r" }
  );
  assert.deepEqual(splitMessage("a🧪b", 2), ["a🧪", "b"]);
  assert.deepEqual(splitMessage("alpha beta gamma", 12), ["alpha beta ", "gamma"]);
  const fenced = splitMessage("```js\nconst first = 1;\nconst second = 2;\n```\nDone", 24);
  assert.ok(fenced.length > 1);
  assert.equal(fenced[0].endsWith("\n```"), true);
  assert.equal(fenced[1].startsWith("```js\n"), true);
  assert.equal(fenced.at(-1).includes("Done"), true);
  for (const chunk of fenced) {
    assert.ok(Array.from(chunk).length <= 24);
    assert.equal((chunk.match(/```/g) || []).length % 2, 0);
  }
  assert.deepEqual(activeTurnBlock({ turns: [{ id: "t1", status: "queued" }] }), {
    turnId: "t1",
    message: "Thread already has active turn t1. Wait for it to finish or send /interrupt."
  });
  assert.deepEqual(activeTurnBlock({ turns: [{ status: "in_progress" }] }, null), {
    turnId: "",
    message: "Thread already has active turn (unknown). Wait for it to finish or send /interrupt."
  });
});

test("ThreadStore supports chat state, message dedupe, and action tokens", async () => {
  const dir = await mkdtemp(path.join(tmpdir(), "codewhale-bridge-core-"));
  try {
    const statePath = path.join(dir, "thread-map.json");
    const store = await ThreadStore.open(statePath, {
      messageLimit: 2,
      actions: true,
      actionLimit: 2
    });

    await store.setChat("chat-a", { threadId: "thread-a" });
    assert.equal((await store.getChat("chat-a")).threadId, "thread-a");

    assert.equal(await store.recordMessage("m1"), false);
    assert.equal(await store.recordMessage("m1"), true);
    assert.equal(await store.recordMessage("m2"), false);
    assert.equal(await store.recordMessage("m3"), false);
    assert.deepEqual(store.data.messages, ["m2", "m3"]);

    const token = await store.putAction({ kind: "resume", threadId: "thread-a" }, { chatId: "chat-a" });
    assert.equal((await store.getAction(token, { chatId: "chat-a" })).kind, "resume");
    assert.equal((await store.takeAction(token, { chatId: "chat-a" })).threadId, "thread-a");
    assert.equal(await store.getAction(token, { chatId: "chat-a" }), null);

    const saved = await ThreadStore.open(statePath, { messageLimit: 2, actions: true });
    assert.equal((await saved.getChat("chat-a")).threadId, "thread-a");
    assert.deepEqual(saved.data.messages, ["m2", "m3"]);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("action tokens remain distinct when clock and legacy randomness repeat", async (t) => {
  const dir = await mkdtemp(path.join(tmpdir(), "codewhale-action-tokens-"));
  try {
    const store = await ThreadStore.open(path.join(dir, "state.json"), { actions: true });
    t.mock.method(Date, "now", () => 1);
    t.mock.method(Math, "random", () => 0.5);
    const first = await store.putAction({ threadId: "thread-a" }, { chatId: "chat-a" });
    const second = await store.putAction({ threadId: "thread-b" }, { chatId: "chat-a" });
    assert.notEqual(first, second);
    assert.match(first, /^[a-f0-9]{32}$/);
    assert.equal((await store.getAction(first, { chatId: "chat-a" })).threadId, "thread-a");
    assert.equal((await store.getAction(second, { chatId: "chat-a" })).threadId, "thread-b");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("readJsonSafe tolerates empty and non-JSON bodies", async () => {
  assert.deepEqual(await readJsonSafe({ text: async () => "" }), {});
  assert.deepEqual(await readJsonSafe({ text: async () => '{"ok":true}' }), { ok: true });
  assert.equal(await readJsonSafe({ text: async () => "plain text" }), "plain text");
});

test("readSse reassembles events split across chunks and strips CR", async () => {
  const response = {
    body: (async function* () {
      yield Buffer.from('event: item.delta\ndata: {"seq":1}\n\nevent:');
      yield Buffer.from(' turn.completed\r\ndata: {"seq":2}\n\n');
    })()
  };
  const events = [];
  for await (const event of readSse(response)) events.push(event);
  assert.deepEqual(events, [
    { event: "item.delta", data: '{"seq":1}' },
    { event: "turn.completed", data: '{"seq":2}' }
  ]);
});

test("createRuntimeClient sends bearer auth and surfaces runtime errors", async () => {
  const calls = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (url, options) => {
    calls.push({ url: String(url), options });
    if (String(url).endsWith("/fail")) {
      return {
        ok: false,
        status: 503,
        text: async () => JSON.stringify({ error: { message: "down" } })
      };
    }
    return { ok: true, status: 200, text: async () => JSON.stringify({ ok: true }) };
  };
  try {
    const { runtimeJson, authHeaders } = createRuntimeClient({
      runtimeUrl: "http://127.0.0.1:7878",
      runtimeToken: "token-1"
    });
    assert.deepEqual(authHeaders(), { authorization: "Bearer token-1" });

    assert.deepEqual(await runtimeJson("/v1/threads", { method: "POST", body: { a: 1 } }), {
      ok: true
    });
    assert.equal(calls[0].url, "http://127.0.0.1:7878/v1/threads");
    assert.equal(calls[0].options.method, "POST");
    assert.equal(calls[0].options.headers.authorization, "Bearer token-1");
    assert.equal(calls[0].options.headers["content-type"], "application/json");
    assert.equal(calls[0].options.body, JSON.stringify({ a: 1 }));

    await runtimeJson("/health", { auth: false });
    assert.equal(calls[1].options.method, "GET");
    assert.deepEqual(calls[1].options.headers, {});

    await assert.rejects(() => runtimeJson("/fail"), /Runtime API request failed \(503\): down/);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("ThreadStore batches rapid saves into coalesced durable writes", async () => {
  const dir = await mkdtemp(path.join(tmpdir(), "codewhale-bridge-core-"));
  try {
    const statePath = path.join(dir, "thread-map.json");
    const store = await ThreadStore.open(statePath);
    let writes = 0;
    const originalWrite = store.writeSnapshot.bind(store);
    store.writeSnapshot = async () => {
      writes += 1;
      return originalWrite();
    };

    await Promise.all(
      Array.from({ length: 25 }, (_, index) =>
        store.setChat(`chat-${index}`, { threadId: `thread-${index}` })
      )
    );
    assert.ok(writes <= 2, `expected coalesced writes, saw ${writes}`);

    const saved = await ThreadStore.open(statePath);
    assert.equal((await saved.getChat("chat-0")).threadId, "thread-0");
    assert.equal((await saved.getChat("chat-24")).threadId, "thread-24");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("ThreadStore persists numeric cursors", async () => {
  const dir = await mkdtemp(path.join(tmpdir(), "codewhale-bridge-core-"));
  try {
    const statePath = path.join(dir, "thread-map.json");
    const store = await ThreadStore.open(statePath);

    assert.equal(store.getCursor("telegram.update_offset", 7), 7);
    assert.equal(await store.setCursor("telegram.update_offset", 42), 42);
    assert.equal(store.getCursor("telegram.update_offset"), 42);

    const saved = await ThreadStore.open(statePath);
    assert.equal(saved.getCursor("telegram.update_offset"), 42);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("writeFileDurable replaces the file through unique temp names and leaves none behind", async () => {
  const dir = await mkdtemp(path.join(tmpdir(), "codewhale-bridge-core-"));
  try {
    const target = path.join(dir, "sync-buf.txt");
    // Concurrent writers used to share one fixed `.tmp` path and race on it.
    await Promise.all(Array.from({ length: 8 }, (_, index) => writeFileDurable(target, `cursor-${index}`)));
    assert.match(await readFile(target, "utf8"), /^cursor-[0-7]$/);
    assert.deepEqual(await readdir(dir), ["sync-buf.txt"]);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("Windows sharing locks retry replacement and preserve old bytes on refusal", { skip: process.platform !== "win32", timeout: 30000 }, async () => {
  const dir = await mkdtemp(path.join(tmpdir(), "codewhale-bridge-lock-"));
  const target = path.join(dir, "state.txt");
  const lockers = [];
  async function lock() {
    const child = spawn("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
      '$file = [System.IO.File]::Open($env:CODEWHALE_TEST_LOCK_FILE, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read); try { [Console]::WriteLine("LOCKED"); [Console]::Out.Flush(); [Console]::ReadLine() | Out-Null } finally { $file.Dispose() }'],
    { env: { ...process.env, CODEWHALE_TEST_LOCK_FILE: target }, stdio: ["pipe", "pipe", "pipe"] });
    lockers.push(child);
    // The child may already have exited when failure cleanup releases its pipe.
    child.stdin.on("error", () => {});
    await new Promise((resolve, reject) => {
      let output = "", errors = "";
      const timeout = setTimeout(() => reject(new Error(`file lock did not open: ${errors}`)), 10000);
      child.once("error", (error) => { clearTimeout(timeout); reject(error); });
      child.once("exit", (code) => { clearTimeout(timeout); reject(new Error(`file lock exited ${code}: ${errors}`)); });
      child.stderr.on("data", (data) => { errors += data; });
      child.stdout.on("data", (data) => {
        output += data;
        if (output.includes("LOCKED")) { clearTimeout(timeout); resolve(); }
      });
    });
    return child;
  }
  try {
    await writeFileDurable(target, "old");
    const transient = await lock();
    const release = setTimeout(() => transient.stdin.end("release\n"), 200);
    try { await writeFileDurable(target, "new"); } finally { clearTimeout(release); if (!transient.stdin.writableEnded) transient.stdin.end("release\n"); }
    assert.equal(await readFile(target, "utf8"), "new");
    assert.deepEqual(await readdir(dir), ["state.txt"]);

    const held = await lock();
    const started = Date.now();
    await assert.rejects(writeFileDurable(target, "must-not-publish"), (error) => ["EPERM", "EACCES", "EBUSY"].includes(error.code));
    assert.ok(Date.now() - started < 10000, "permanent refusal must stay bounded");
    assert.equal(await readFile(target, "utf8"), "new", "failed publication retains the old record");
    assert.deepEqual(await readdir(dir), ["state.txt"], "failed publication cleans up its temporary file");
    held.stdin.end("release\n");
  } finally {
    await Promise.all(lockers.map((child) => new Promise((resolve) => {
      if (child.exitCode !== null || child.signalCode !== null) return resolve();
      child.once("exit", resolve);
      if (!child.stdin.writableEnded) child.stdin.end("release\n");
      child.kill();
    })));
    await rm(dir, { recursive: true, force: true });
  }
});

test("ThreadStore claims survive a restart: finished messages skip, an in-flight one is reported", async () => {
  const dir = await mkdtemp(path.join(tmpdir(), "codewhale-bridge-core-"));
  try {
    const statePath = path.join(dir, "thread-map.json");
    const store = await ThreadStore.open(statePath, { messageLimit: 10 });
    assert.equal(await store.claimMessage("u:1"), "new");
    await store.completeMessage("u:1");
    assert.equal(await store.claimMessage("u:2"), "new");
    // The process dies here, before completeMessage("u:2").
    const restarted = await ThreadStore.open(statePath, { messageLimit: 10 });
    assert.equal(await restarted.claimMessage("u:1"), "done");
    assert.equal(await restarted.claimMessage("u:2"), "interrupted");
    assert.equal(await restarted.claimMessage("u:2"), "interrupted", "keep the claim until the report succeeds");
    await restarted.completeMessage("u:2");
    assert.equal(await restarted.claimMessage("u:2"), "done", "reported once, then an ordinary duplicate");
    assert.equal(await restarted.claimMessage("u:3"), "new");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
