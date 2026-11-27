import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { SseParser, parseFrame, type RuntimeEvent } from "../sse";

function eventsOf(chunks: string[]): RuntimeEvent[] {
  const parser = new SseParser();
  return chunks.flatMap((chunk) => parser.push(chunk));
}

describe("SseParser", () => {
  it("parses a single frame with the runtime envelope", () => {
    const events = eventsOf([
      'data: {"seq":42,"previous_seq":38,"event":"item.delta","thread_id":"t1","turn_id":"turn_1","item_id":"i1","timestamp":"2026-02-11T20:18:49.123Z","payload":{"delta":"partial output","kind":"agent_message"}}\n\n',
    ]);
    assert.equal(events.length, 1);
    assert.equal(events[0].seq, 42);
    assert.equal(events[0].previousSeq, 38);
    assert.equal(events[0].event, "item.delta");
    assert.equal(events[0].threadId, "t1");
    assert.equal(events[0].turnId, "turn_1");
    assert.equal(events[0].itemId, "i1");
    assert.deepEqual(events[0].payload, { delta: "partial output", kind: "agent_message" });
  });

  it("parses several frames in one chunk", () => {
    const chunk =
      'data: {"seq":1,"event":"turn.started"}\n\n' +
      'data: {"seq":2,"event":"item.started"}\n\n' +
      'data: {"seq":3,"event":"turn.completed"}\n\n';
    const events = eventsOf([chunk]);
    assert.deepEqual(
      events.map((event) => event.seq),
      [1, 2, 3],
    );
  });

  it("reassembles a frame split across chunks", () => {
    const events = eventsOf([
      'data: {"seq":7,',
      '"event":"item.delta"}',
      "\n\n",
    ]);
    assert.equal(events.length, 1);
    assert.equal(events[0].seq, 7);
    assert.equal(events[0].event, "item.delta");
  });

  it("handles CRLF frame boundaries", () => {
    const events = eventsOf(['data: {"seq":9,"event":"turn.started"}\r\n\r\n']);
    assert.equal(events.length, 1);
    assert.equal(events[0].seq, 9);
  });

  it("ignores comments and heartbeats", () => {
    const events = eventsOf([": heartbeat\n\n", ": keep-alive\n\n"]);
    assert.equal(events.length, 0);
  });

  it("ignores frames without valid JSON", () => {
    const events = eventsOf(["data: not-json\n\n"]);
    assert.equal(events.length, 0);
  });

  it("joins multi-line data fields", () => {
    // Two `data:` lines join with \n; the joined payload is valid JSON here.
    const events = eventsOf(['data: {"seq":1,\ndata: "event":"x"}\n\n']);
    assert.equal(events.length, 1);
    assert.equal(events[0].seq, 1);
    assert.equal(events[0].event, "x");
    const good = parseFrame('data: {"seq":1,\ndata: "x":1}\n\n');
    assert.equal(good, undefined); // parses, but has no event name
  });

  it("drops events without seq or event name", () => {
    assert.equal(eventsOf(['data: {"event":"no-seq"}\n\n']).length, 0);
    assert.equal(eventsOf(['data: {"seq":1}\n\n']).length, 0);
  });
  it("refuses an unterminated frame that outgrows the limit", () => {
    const parser = new SseParser(64);
    assert.deepEqual(parser.push("data: " + "x".repeat(32)), []);
    assert.throws(() => parser.push("x".repeat(64)), /size limit/);
    assert.equal(parser.push('data: {"seq":1,"event":"item.delta"}\n\n').length, 1);
  });
});
