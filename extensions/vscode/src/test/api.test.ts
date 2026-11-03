import assert from "node:assert/strict";
import * as http from "node:http";
import type { AddressInfo } from "node:net";
import { after, before, describe, it } from "node:test";
import {
  answerUserInput,
  ApiError,
  checkConnection,
  ConflictError,
  getThreadDetail,
  interruptTurn,
  listThreadSummaries,
  openEventStream,
  resolveApproval,
  startTurn,
  steerTurn,
  type ApiConfig,
  type RuntimeEvent,
} from "../api";

describe("api client", () => {
  let server: http.Server;
  let baseUrl: string;
  const seen: { path?: string; auth?: string; method?: string; body?: string } = {};

  before(async () => {
    server = http.createServer((request, response) => {
      let body = "";
      request.on("data", (chunk: Buffer) => {
        body += chunk.toString("utf8");
      });
      request.on("end", () => {
        seen.path = request.url;
        seen.auth = request.headers.authorization;
        seen.method = request.method;
        seen.body = body;

        if (request.url === "/health") {
          response.writeHead(200, { "Content-Type": "application/json" });
          response.end("{}");
          return;
        }
        if (request.url === "/v1/runtime/info") {
          response.writeHead(200, { "Content-Type": "application/json" });
          response.end(JSON.stringify({ version: "0.9.12", auth_required: true }));
          return;
        }
        if (request.url?.startsWith("/v1/threads/summary")) {
          if (seen.auth !== "Bearer sekrit") {
            response.writeHead(401, { "Content-Type": "application/json" });
            response.end("{}");
            return;
          }
          response.writeHead(200, { "Content-Type": "application/json" });
          response.end(
            JSON.stringify([
              {
                id: "thr_1",
                title: "Implement chat",
                preview: "Let me start…",
                model: "deepseek-v4-pro",
                mode: "agent",
                branch: "main",
                head: "abc1234",
                dirty: true,
                archived: false,
                updated_at: "2026-06-06T05:43:00Z",
                latest_turn_status: "completed",
              },
            ]),
          );
          return;
        }
        if (request.url === "/v1/threads/thr_1" && request.method === "GET") {
          response.writeHead(200, { "Content-Type": "application/json" });
          response.end(
            JSON.stringify({
              thread: { id: "thr_1", model: "deepseek-v4-pro", updated_at: "2026-06-06T05:43:00Z" },
              turns: [{ id: "turn_1", status: "completed" }],
              items: [
                { id: "item_1", turn_id: "turn_1", kind: "user_message", status: "completed", summary: "hi" },
                {
                  id: "item_2",
                  turn_id: "turn_1",
                  kind: "agent_message",
                  status: "completed",
                  summary: "**done**",
                },
              ],
              latest_seq: 12,
              pending_approvals: [
                { id: "ap_1", turn_id: "turn_1", tool_name: "shell", description: "rm -rf /" },
              ],
              pending_user_inputs: [
                {
                  id: "ui_1",
                  turn_id: "turn_1",
                  request: {
                    questions: [
                      {
                        header: "Approach",
                        id: "q1",
                        question: "Which way?",
                        options: [{ label: "Fast", description: "quick" }],
                        allow_free_text: true,
                      },
                    ],
                  },
                },
              ],
            }),
          );
          return;
        }
        if (request.url === "/v1/threads/thr_1/turns" && request.method === "POST") {
          if (!body.includes("operation_key")) {
            response.writeHead(400, { "Content-Type": "application/json" });
            response.end("{}");
            return;
          }
          // The real contract: `start_thread_turn` answers 201 CREATED
          // (crates/app-server/src/runtime_api.rs:4613-4631).
          response.writeHead(201, { "Content-Type": "application/json" });
          response.end(
            JSON.stringify({
              thread: { id: "thr_1" },
              turn: { id: "turn_2", status: "queued" },
            }),
          );
          return;
        }
        // Every 2xx is a success, so pin the edges of the range as well.
        const rangeMatch = /^\/v1\/threads\/thr_(200|201|202)\/turns$/.exec(request.url ?? "");
        if (rangeMatch && request.method === "POST") {
          response.writeHead(Number(rangeMatch[1]), { "Content-Type": "application/json" });
          response.end(
            JSON.stringify({
              thread: { id: `thr_${rangeMatch[1]}` },
              turn: { id: "turn_range", status: "queued" },
            }),
          );
          return;
        }
        if (request.url === "/v1/threads/thr_500/turns") {
          response.writeHead(500, { "Content-Type": "application/json" });
          response.end(JSON.stringify({ error: { code: "internal", message: "engine exploded" } }));
          return;
        }
        if (request.url === "/v1/threads/thr_401/turns") {
          response.writeHead(401, { "Content-Type": "application/json" });
          response.end(JSON.stringify({ error: { message: "missing bearer token" } }));
          return;
        }
        if (request.url === "/v1/threads/thr_1/turns/turn_idle/interrupt") {
          response.writeHead(409, { "Content-Type": "application/json" });
          response.end(JSON.stringify({ error: { message: "No active turn for thread 'thr_1'" } }));
          return;
        }
        if (request.url === "/v1/threads/thr_1/turns/turn_2/steer") {
          response.writeHead(202, { "Content-Type": "application/json" });
          response.end("{}");
          return;
        }
        if (request.url === "/v1/threads/thr_1/turns/turn_2/interrupt") {
          response.writeHead(202, { "Content-Type": "application/json" });
          response.end("{}");
          return;
        }
        if (request.url === "/v1/approvals/ap_1") {
          response.writeHead(200, { "Content-Type": "application/json" });
          response.end(JSON.stringify({ decision: JSON.parse(body).decision }));
          return;
        }
        if (request.url === "/v1/user-input/thr_1/ui_1") {
          response.writeHead(200, { "Content-Type": "application/json" });
          response.end("{}");
          return;
        }
        if (request.url === "/v1/threads/thr_conflict/turns") {
          response.writeHead(409, { "Content-Type": "application/json" });
          response.end(JSON.stringify({ error: { code: "operation_key_conflict", message: "key reuse" } }));
          return;
        }
        response.writeHead(404, { "Content-Type": "application/json" });
        response.end("{}");
      });
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    baseUrl = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  });

  after(() => {
    server.close();
  });

  const config = (token?: string): ApiConfig => ({ baseUrl, token });

  it("reports connected with version", async () => {
    const info = await checkConnection(config("sekrit"));
    assert.equal(info.kind, "connected");
    assert.equal(info.version, "0.9.12");
  });

  it("reports auth-required when info demands a token and none is set", async () => {
    const strict = http.createServer((request, response) => {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ auth_required: true }));
      request.on("data", () => undefined);
    });
    await new Promise<void>((resolve) => strict.listen(0, "127.0.0.1", resolve));
    const strictBase = `http://127.0.0.1:${(strict.address() as AddressInfo).port}`;
    const info = await checkConnection({ baseUrl: strictBase });
    assert.equal(info.kind, "auth-required");
    strict.close();
  });

  it("lists thread summaries with the bearer token", async () => {
    const threads = await listThreadSummaries(config("sekrit"));
    assert.equal(seen.auth, "Bearer sekrit");
    assert.equal(threads.length, 1);
    assert.equal(threads[0].id, "thr_1");
    assert.equal(threads[0].dirty, true);
  });

  it("hydrates thread detail with pending approvals and inputs", async () => {
    const detail = await getThreadDetail(config(), "thr_1");
    assert.equal(detail.latestSeq, 12);
    assert.equal(detail.items.length, 2);
    assert.equal(detail.pendingApprovals[0].toolName, "shell");
    assert.equal(detail.pendingUserInputs[0].questions[0].options[0].label, "Fast");
    assert.equal(detail.pendingUserInputs[0].questions[0].allowFreeText, true);
  });

  it("starts a turn with an idempotency key and accepts the runtime's 201", async () => {
    const result = await startTurn(config(), "thr_1", {
      prompt: "do the thing",
      operationKey: "op-123",
    });
    assert.ok(seen.body?.includes("operation_key"));
    assert.ok(seen.body?.includes("do the thing"));
    assert.equal(result.turn.id, "turn_2");
  });

  it("accepts every 2xx, not a hand-picked list of codes", async () => {
    for (const status of ["200", "201", "202"]) {
      const result = await startTurn(config(), `thr_${status}`, { prompt: "x", operationKey: "k" });
      assert.equal(result.turn.id, "turn_range", `HTTP ${status} should be a success`);
    }
  });

  it("surfaces the runtime's message on a 5xx", async () => {
    await assert.rejects(
      startTurn(config(), "thr_500", { prompt: "x", operationKey: "k" }),
      (error: unknown) => {
        assert.ok(error instanceof ApiError);
        assert.equal((error as ApiError).statusCode, 500);
        assert.ok((error as ApiError).message.includes("engine exploded"));
        return true;
      },
    );
  });

  it("signals auth-required on a mutating 401", async () => {
    await assert.rejects(
      startTurn(config(), "thr_401", { prompt: "x", operationKey: "k" }),
      (error: unknown) => {
        assert.ok(error instanceof ApiError);
        assert.equal((error as ApiError).statusCode, 401);
        assert.ok((error as ApiError).message.includes("requires the runtime token"));
        return true;
      },
    );
  });

  it("steers and interrupts", async () => {
    await steerTurn(config(), "thr_1", "turn_2", "focus on tests");
    assert.ok(seen.path?.includes("/steer"));
    assert.ok(seen.body?.includes("focus on tests"));
    const result = await interruptTurn(config(), "thr_1", "turn_2");
    assert.ok(seen.path?.endsWith("/interrupt"));
    assert.equal(result, "interrupted");
  });

  it("treats a 409 interrupt as nothing to stop, not a failure", async () => {
    const result = await interruptTurn(config(), "thr_1", "turn_idle");
    assert.equal(result, "not-running");
  });

  it("resolves approvals and answers user input", async () => {
    await resolveApproval(config(), "ap_1", "deny", true);
    assert.ok(seen.body?.includes('"deny"'));
    assert.ok(seen.body?.includes('"remember":true'));
    await answerUserInput(config(), "thr_1", "ui_1", [{ id: "q1", label: "Fast", value: "Fast" }]);
    assert.ok(seen.body?.includes("ui_1") || seen.path?.includes("ui_1"));
    assert.ok(seen.body?.includes("Fast"));
  });

  it("surfaces 409 as a typed conflict with server detail", async () => {
    const failing: ApiConfig = { baseUrl };
    const promise = startTurn(failing, "thr_conflict", { prompt: "x", operationKey: "k" });
    await assert.rejects(promise, (error: unknown) => {
      assert.ok(error instanceof ApiError);
      // A conflict is its own signal so callers can say "a turn is already
      // running" rather than reporting a generic HTTP failure.
      assert.ok(error instanceof ConflictError);
      assert.equal((error as ApiError).statusCode, 409);
      assert.equal((error as ApiError).detail, "key reuse");
      assert.ok((error as ApiError).message.includes("key reuse"));
      return true;
    });
  });

  it("streams and parses SSE events", async () => {
    const received: RuntimeEvent[] = [];
    let finish!: () => void;
    const done = new Promise<void>((resolve) => {
      finish = resolve;
    });

    const sseServer = http.createServer((request, response) => {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      response.write('data: {"seq":1,"event":"item.started","item_id":"i1","payload":{"kind":"agent_message"}}\n\n');
      response.write('data: {"seq":2,"event":"item.delta","item_id":"i1","payload":{"delta":"hel"}}\n\n');
      setTimeout(() => {
        response.write('data: {"seq":3,"event":"item.completed","item_id":"i1","payload":{"summary":"hello"}}\n\n');
        response.end();
      }, 20);
    });
    await new Promise<void>((resolve) => sseServer.listen(0, "127.0.0.1", resolve));
    const sseBase = `http://127.0.0.1:${(sseServer.address() as AddressInfo).port}`;

    const stream = openEventStream({ baseUrl: sseBase }, "thr_9", 0);
    let streamError: Error | undefined;
    stream.onEvent = (event) => {
      received.push(event);
      if (received.length === 3) {
        finish();
      }
    };
    // The server ends the response after the third event, so a trailing
    // "stream closed" error is expected and must not loop or throw.
    stream.onError = (error) => {
      streamError = error;
    };
    await done;
    assert.deepEqual(
      received.map((event) => [event.seq, event.event]),
      [
        [1, "item.started"],
        [2, "item.delta"],
        [3, "item.completed"],
      ],
    );
    assert.ok(streamError);
    stream.close();
    sseServer.close();
  });
});
