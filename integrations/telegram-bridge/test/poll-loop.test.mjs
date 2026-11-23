import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function message(updateId, chatId, text) {
  return {
    update_id: updateId,
    message: {
      message_id: updateId,
      date: 0,
      chat: { id: chatId, type: "private" },
      from: { id: chatId, is_bot: false, first_name: "user" },
      text
    }
  };
}

test("an update whose reply is permanently refused does not block later updates", async (t) => {
  const pending = [message(500, 999, "hi"), message(501, 111, "/help")];
  const stats = { getUpdates: 0, sendTo999: 0, sendTo111: 0 };
  const server = http.createServer((req, res) => {
    let raw = "";
    req.on("data", (chunk) => (raw += chunk));
    req.on("end", () => {
      const body = raw ? JSON.parse(raw) : {};
      const method = req.url.split("/").pop();
      const reply = (status, payload) => {
        res.writeHead(status, { "content-type": "application/json" });
        res.end(JSON.stringify(payload));
      };
      if (method === "getUpdates") {
        stats.getUpdates += 1;
        const result = pending.filter((update) => update.update_id >= (body.offset || 0));
        if (result.length) return reply(200, { ok: true, result });
        return setTimeout(() => reply(200, { ok: true, result: [] }), 200);
      }
      if (method === "sendMessage" && Number(body.chat_id) === 999) {
        stats.sendTo999 += 1;
        return reply(403, { ok: false, error_code: 403, description: "Forbidden: bot was blocked by the user" });
      }
      if (method === "sendMessage") {
        stats.sendTo111 += 1;
        return reply(200, { ok: true, result: { message_id: 1, chat: { id: body.chat_id } } });
      }
      return reply(200, { ok: true, result: true });
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const stateDir = await fs.mkdtemp(path.join(os.tmpdir(), "telegram-poll-loop-"));
  const bridge = spawn(process.execPath, [path.join(__dirname, "../src/index.mjs")], {
    env: {
      ...process.env,
      TELEGRAM_BOT_TOKEN: "123:test",
      TELEGRAM_API_BASE: `http://127.0.0.1:${server.address().port}`,
      TELEGRAM_CHAT_ALLOWLIST: "111",
      TELEGRAM_THREAD_MAP_PATH: path.join(stateDir, "thread-map.json"),
      TELEGRAM_POLL_TIMEOUT_SECONDS: "1",
      CODEWHALE_RUNTIME_URL: "http://127.0.0.1:9",
      CODEWHALE_RUNTIME_TOKEN: "test"
    },
    stdio: "ignore"
  });
  t.after(async () => {
    bridge.kill("SIGKILL");
    server.close();
    await fs.rm(stateDir, { recursive: true, force: true });
  });

  const deadline = Date.now() + 15000;
  while (stats.sendTo111 === 0 && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }

  assert.equal(stats.sendTo111, 1, `later update was never handled: ${JSON.stringify(stats)}`);
  assert.equal(stats.sendTo999, 1, `refused update was retried: ${JSON.stringify(stats)}`);
});
