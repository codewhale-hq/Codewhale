/**
 * Minimal text/event-stream parser for `GET /v1/threads/{id}/events`.
 *
 * Pure and dependency-free: callers feed raw network chunks in, parsed frames
 * come out. The Codewhale runtime encodes each event as a JSON object in the
 * frame data, so a frame with parseable JSON yields one RuntimeEvent; frames
 * without JSON (heartbeats, comments) yield nothing.
 */

export interface RuntimeEvent {
  seq: number;
  previousSeq?: number;
  event: string;
  threadId?: string;
  turnId?: string;
  itemId?: string;
  timestamp?: string;
  payload: unknown;
}

export const MAX_SSE_FRAME_CHARS = 2 * 1024 * 1024;

export class SseParser {
  private buffer = "";

  constructor(private readonly maxFrameChars = MAX_SSE_FRAME_CHARS) {}

  /** Feed one raw chunk; returns the complete events it finished. Throws when an unterminated frame outgrows the limit. */
  push(chunk: string): RuntimeEvent[] {
    this.buffer += chunk;
    const events: RuntimeEvent[] = [];
    let boundary = this.nextBoundary();
    while (boundary !== -1) {
      const frame = this.buffer.slice(0, boundary.index);
      this.buffer = this.buffer.slice(boundary.index + boundary.length);
      const event = parseFrame(frame);
      if (event) {
        events.push(event);
      }
      boundary = this.nextBoundary();
    }
    if (this.buffer.length > this.maxFrameChars) {
      this.buffer = "";
      throw new Error("Runtime event frame exceeds the size limit.");
    }
    return events;
  }

  private nextBoundary(): { index: number; length: number } | -1 {
    const lf = this.buffer.indexOf("\n\n");
    const crlf = this.buffer.indexOf("\r\n\r\n");
    if (lf === -1 && crlf === -1) {
      return -1;
    }
    if (crlf === -1 || (lf !== -1 && lf < crlf)) {
      return { index: lf, length: 2 };
    }
    return { index: crlf, length: 4 };
  }
}

export function parseFrame(frame: string): RuntimeEvent | undefined {
  let data = "";
  for (const rawLine of frame.split(/\r?\n/)) {
    if (rawLine === "" || rawLine.startsWith(":")) {
      continue;
    }
    const colon = rawLine.indexOf(":");
    const field = colon === -1 ? rawLine : rawLine.slice(0, colon);
    let value = colon === -1 ? "" : rawLine.slice(colon + 1);
    if (value.startsWith(" ")) {
      value = value.slice(1);
    }
    if (field === "data") {
      data += (data ? "\n" : "") + value;
    }
    // `event`, `id`, and `retry` are ignored: the runtime envelope carries the
    // event name in its own `event` field, which is the stable contract.
  }
  if (!data) {
    return undefined;
  }
  return readEvent(data);
}

function readEvent(data: string): RuntimeEvent | undefined {
  let body: unknown;
  try {
    body = JSON.parse(data);
  } catch {
    return undefined;
  }
  if (!body || typeof body !== "object") {
    return undefined;
  }
  const record = body as Record<string, unknown>;
  const seq = readNumber(record.seq);
  const event = readString(record.event);
  if (seq === undefined || !event) {
    return undefined;
  }
  return {
    seq,
    previousSeq: readNumber(record.previous_seq),
    event,
    threadId: readString(record.thread_id),
    turnId: readString(record.turn_id),
    itemId: readString(record.item_id),
    timestamp: readString(record.timestamp) ?? readString(record.created_at),
    payload: record.payload,
  };
}

function readString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function readNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}
