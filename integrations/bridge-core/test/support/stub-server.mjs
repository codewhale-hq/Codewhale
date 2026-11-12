import http from "node:http";

export async function startStub() {
  const state = {
    threads: [],
    nextThread: 1,
    nextTurn: 1,
    runtimeRequests: [],
    replies: [],
    inbound: { wecom: [], feishu: [], telegram: [] },
    nextUpdateId: 1
  };

  function thread(id) {
    return state.threads.find((entry) => entry.id === id) || null;
  }

  function seedThread({ id, workspace, summary, runningTurnId }) {
    const entry = { id, workspace, turns: [], events: [], prompts: [], latestSummary: summary || "", createdBy: "seed" };
    if (summary) entry.prompts.push(summary);
    if (runningTurnId) entry.turns.push({ id: runningTurnId, status: "in_progress" });
    state.threads.unshift(entry);
    return entry;
  }

  function json(res, status, body) {
    res.writeHead(status, { "content-type": "application/json" });
    res.end(JSON.stringify(body));
  }

  async function readBody(req) {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const raw = Buffer.concat(chunks).toString("utf8");
    return raw ? JSON.parse(raw) : null;
  }

  function runtime(req, res, url, body) {
    const route = `${req.method} ${url.pathname}`;
    state.runtimeRequests.push({ route, body, query: url.search });
    if (route === "GET /health") return json(res, 200, { status: "ok" });
    if (route === "GET /v1/runtime/info") return json(res, 200, { version: "fake", bind_host: "127.0.0.1", port: 0, auth_required: true, capabilities: {} });
    if (route === "GET /v1/workspace/status") return json(res, 200, { workspace: "/fake", git_repo: false });
    if (route === "POST /v1/threads") {
      const entry = { id: `thr-${state.nextThread++}`, workspace: body?.workspace || "", turns: [], events: [], prompts: [], latestSummary: "", createdBy: "bridge" };
      state.threads.unshift(entry);
      return json(res, 200, { id: entry.id });
    }
    if (route === "GET /v1/threads/summary") {
      const limit = Number(url.searchParams.get("limit") || 50);
      const ids = url.searchParams.get("thread_ids");
      const wanted = ids == null ? null : new Set(ids.split(","));
      const rows = state.threads
        .filter((entry) => !wanted || wanted.has(entry.id))
        .slice(0, limit)
        .map((entry) => ({
          id: entry.id,
          title: entry.latestSummary ? entry.latestSummary.slice(0, 72) : "New Thread",
          workspace: entry.workspace,
          latest_turn_status: entry.turns.at(-1)?.status || null
        }));
      return json(res, 200, rows);
    }
    const match = url.pathname.match(/^\/v1\/threads\/([^/]+)(?:\/(.*))?$/);
    if (!match) return json(res, 404, { message: "no route" });
    const entry = thread(decodeURIComponent(match[1]));
    if (!entry) return json(res, 404, { message: "thread not found" });
    const rest = match[2] || "";
    if (req.method === "GET" && rest === "") {
      return json(res, 200, { id: entry.id, latest_seq: entry.events.length, turns: entry.turns, pending_approvals: [] });
    }
    if (req.method === "POST" && rest === "turns") {
      const turn = { id: `turn-${state.nextTurn++}`, thread_id: entry.id, status: "completed" };
      const history = entry.prompts.join(" | ");
      entry.prompts.push(body.prompt);
      entry.latestSummary = body.input_summary || body.prompt;
      entry.turns.push(turn);
      const base = entry.events.length;
      entry.events.push({ seq: base + 1, thread_id: entry.id, turn_id: turn.id, event: "item.delta", payload: { kind: "agent_message", delta: `STUB-MODEL thread=${entry.id} earlier-inputs=[${history}]` } });
      entry.events.push({ seq: base + 2, thread_id: entry.id, turn_id: turn.id, event: "turn.completed", payload: { turn: { status: "completed" } } });
      return json(res, 200, { turn });
    }
    if (req.method === "GET" && rest === "events") {
      const since = Number(url.searchParams.get("since_seq") || 0);
      res.writeHead(200, { "content-type": "text/event-stream" });
      for (const record of entry.events.filter((event) => event.seq > since)) {
        res.write(`data: ${JSON.stringify(record)}\n\n`);
      }
      res.end(`data: ${JSON.stringify({ thread_id: entry.id, event: "stream.end" })}\n\n`);
      return;
    }
    if (req.method === "POST" && /^turns\/[^/]+\/(steer|interrupt)$/.test(rest)) return json(res, 200, { ok: true });
    if (req.method === "POST" && rest === "compact") return json(res, 200, { turn: { id: "turn-compact" } });
    return json(res, 404, { message: "no route" });
  }

  function telegram(req, res, method, body) {
    if (method === "getUpdates") {
      const offset = Number(body?.offset || 0);
      const deliver = () => json(res, 200, { ok: true, result: state.inbound.telegram.filter((update) => update.update_id >= offset) });
      if (state.inbound.telegram.some((update) => update.update_id >= offset)) return deliver();
      setTimeout(deliver, 150);
      return;
    }
    if (method === "sendMessage") {
      state.replies.push({ platform: "telegram", to: String(body.chat_id), text: String(body.text).replace(/\\(.)/g, "$1") });
      return json(res, 200, { ok: true, result: { message_id: Date.now() } });
    }
    return json(res, 200, { ok: true, result: true });
  }

  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url, "http://127.0.0.1");
      const body = req.method === "POST" ? await readBody(req) : null;
      if (url.pathname.startsWith("/tg/")) {
        return telegram(req, res, url.pathname.split("/").at(-1), body);
      }
      if (url.pathname === "/fake/next") {
        const queue = state.inbound[url.searchParams.get("platform")] || [];
        return json(res, 200, queue.splice(0, queue.length));
      }
      if (url.pathname === "/fake/reply") {
        state.replies.push(body);
        return json(res, 200, { ok: true });
      }
      return runtime(req, res, url, body);
    } catch (error) {
      json(res, 500, { message: String(error?.message || error) });
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${server.address().port}`;

  return {
    url,
    state,
    seedThread,
    thread,
    pushWecom(userId, text) {
      state.inbound.wecom.push({ body: { chattype: "single", from: { userid: userId }, msgid: `m-${Math.random()}`, msgtype: "text", text: { content: text } } });
    },
    pushFeishu(openId, chatId, text) {
      state.inbound.feishu.push({
        sender: { sender_id: { open_id: openId } },
        message: { chat_id: chatId, message_id: `m-${Math.random()}`, chat_type: "p2p", message_type: "text", content: JSON.stringify({ text }) }
      });
    },
    pushTelegram(userId, text) {
      const update_id = state.nextUpdateId++;
      state.inbound.telegram.push({ update_id, message: { message_id: update_id, chat: { id: userId, type: "private" }, from: { id: userId, is_bot: false }, text } });
    },
    close: () => new Promise((resolve) => { server.closeAllConnections?.(); server.close(resolve); })
  };
}
