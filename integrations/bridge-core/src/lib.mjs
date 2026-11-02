import { chmod, mkdir, open, readFile, rename, rm } from "node:fs/promises";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";

const DEFAULT_ACTION_TTL_MS = 24 * 60 * 60 * 1000;

function normalizeCursorValue(value, fallback = 0) {
  const number = Number(value);
  if (Number.isFinite(number) && number >= 0) return Math.floor(number);
  const fallbackNumber = Number(fallback);
  if (Number.isFinite(fallbackNumber) && fallbackNumber >= 0) return Math.floor(fallbackNumber);
  return 0;
}

async function chmodBestEffort(filePath, mode) {
  try {
    await chmod(filePath, mode);
  } catch (error) {
    if (process.platform !== "win32") throw error;
  }
}

/**
 * Replace `filePath` so a crash leaves either the old bytes or the new ones:
 * a unique temporary name (two processes sharing a state dir never write the
 * same temp file), fsync before the rename, and a best-effort directory fsync
 * after it so the rename itself survives power loss where the OS allows.
 */
export async function writeFileDurable(filePath, contents, { mode = 0o600 } = {}) {
  const dir = path.dirname(filePath);
  const tmp = path.join(dir, `.${path.basename(filePath)}.${process.pid}.${randomBytes(6).toString("hex")}.tmp`);
  let handle = null;
  try {
    handle = await open(tmp, "wx", mode);
    await handle.writeFile(contents);
    await handle.sync();
    await handle.close();
    handle = null;
    for (let attempt = 0; ; attempt++) {
      try {
        await rename(tmp, filePath);
        break;
      } catch (error) {
        // Windows readers/other writers can temporarily deny replacement.
        // Never unlink the old record to make the rename succeed.
        if (process.platform !== "win32" || !["EPERM", "EACCES", "EBUSY"].includes(error.code) || attempt >= 10) throw error;
        await delay(50 * (attempt + 1));
      }
    }
  } catch (error) {
    await handle?.close().catch(() => {});
    await rm(tmp, { force: true }).catch(() => {});
    throw error;
  }
  let dirHandle = null;
  try {
    dirHandle = await open(dir, "r");
    await dirHandle.sync();
  } catch {
    // Windows and some filesystems cannot fsync a directory.
  } finally {
    await dirHandle?.close().catch(() => {});
  }
}

export class ThreadStore {
  static async open(filePath, options = {}) {
    const store = new this(filePath, options);
    await store.load();
    return store;
  }

  constructor(filePath, options = {}) {
    this.filePath = filePath;
    this.options = {
      messageLimit: options.messageLimit || 0,
      actions: options.actions === true,
      actionLimit: options.actionLimit || 200,
      actionTtlMs: options.actionTtlMs || DEFAULT_ACTION_TTL_MS,
      privateMode: options.privateMode === true
    };
    this.data = { chats: {} };
    this.saveDirty = false;
    this.savePending = null;
    this.handlingMessages = new Map();
    this.ensureShape();
  }

  ensureShape() {
    if (!this.data || typeof this.data !== "object") this.data = {};
    if (!this.data.chats || typeof this.data.chats !== "object") this.data.chats = {};
    if (this.options.messageLimit > 0 && !Array.isArray(this.data.messages)) {
      this.data.messages = [];
    }
    if (this.options.messageLimit > 0 && (!this.data.inflight || typeof this.data.inflight !== "object" || Array.isArray(this.data.inflight))) {
      this.data.inflight = {};
    }
    if (this.options.actions && (!this.data.actions || typeof this.data.actions !== "object")) {
      this.data.actions = {};
    }
    if (this.data.cursors && typeof this.data.cursors !== "object") {
      this.data.cursors = {};
    }
  }

  async load() {
    try {
      const raw = await readFile(this.filePath, "utf8");
      this.data = JSON.parse(raw);
      this.ensureShape();
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }

  /**
   * Claim one inbound message before acting on it. "new": handle it now (it
   * is recorded as in flight first, durably). "done": a finished duplicate.
   * "interrupted": an earlier process claimed it and stopped before
   * completeMessage. The caller reports the unknown outcome and completes
   * the claim after the report succeeds.
   */
  async claimMessage(messageKey) {
    if (!messageKey || this.options.messageLimit <= 0) return "new";
    this.ensureShape();
    const inflight = this.data.inflight;
    if (Object.hasOwn(inflight, messageKey)) return "interrupted";
    if (this.data.messages.includes(messageKey)) return "done";
    if (Object.keys(inflight).length >= this.options.messageLimit) {
      throw new Error("Too many unfinished bridge messages. Reconcile their outcomes before accepting more.");
    }
    this.data.inflight = { ...inflight, [messageKey]: new Date().toISOString() };
    await this.save();
    return "new";
  }

  /** Mark a claimed message handled after its action or interruption report finishes. */
  async completeMessage(messageKey) {
    if (!messageKey || !this.data.inflight || !Object.hasOwn(this.data.inflight, messageKey)) return;
    delete this.data.inflight[messageKey];
    if (!this.data.messages.includes(messageKey)) {
      this.data.messages.push(messageKey);
      this.data.messages = this.data.messages.slice(-this.options.messageLimit);
    }
    await this.save();
  }

  async handleMessageOnce(messageKey, handle, reportInterrupted) {
    if (!messageKey || this.options.messageLimit <= 0) return handle();
    if (this.handlingMessages.has(messageKey)) return this.handlingMessages.get(messageKey);
    const pending = Promise.resolve().then(async () => {
      const claim = await this.claimMessage(messageKey);
      if (claim === "done") return;
      if (claim === "interrupted") await reportInterrupted();
      else await handle();
      await this.completeMessage(messageKey);
    }).finally(() => this.handlingMessages.delete(messageKey));
    this.handlingMessages.set(messageKey, pending);
    return pending;
  }

  async recordMessage(messageKey, chatId = null, patch = null) {
    if (patch && !chatId) throw new Error("Durable admission requires a chat binding");
    if (!messageKey || this.options.messageLimit <= 0) {
      if (patch) throw new Error("Durable admission requires message retention");
      return false;
    }
    this.ensureShape();
    if (this.data.messages.includes(messageKey)) return true;
    this.data.messages.push(messageKey);
    this.data.messages = this.data.messages.slice(-this.options.messageLimit);
    if (chatId && patch) this.data.chats[chatId] = { ...this.data.chats[chatId], ...patch };
    await this.save();
    return false;
  }

  getCursor(name, fallback = 0) {
    if (!name) return normalizeCursorValue(fallback);
    this.ensureShape();
    return normalizeCursorValue(this.data.cursors?.[name], fallback);
  }

  async setCursor(name, value) {
    if (!name) return normalizeCursorValue(value);
    this.ensureShape();
    if (!this.data.cursors || typeof this.data.cursors !== "object") {
      this.data.cursors = {};
    }
    const cursor = normalizeCursorValue(value);
    if (this.data.cursors[name] === cursor) return cursor;
    this.data.cursors[name] = cursor;
    await this.save();
    return cursor;
  }

  async getChat(chatId) {
    return this.data.chats[chatId] || null;
  }

  listChats() {
    return Object.entries(this.data.chats || {});
  }

  async setChat(chatId, state) {
    this.data.chats[chatId] = state;
    await this.save();
    return state;
  }

  async patchChat(chatId, patch) {
    const current = this.data.chats[chatId] || {};
    this.data.chats[chatId] = { ...current, ...patch };
    await this.save();
    return this.data.chats[chatId];
  }

  async finishObservedTurn(chatId, threadId, turnId) {
    const current = this.data.chats[chatId];
    if (turnId && current?.threadId === threadId && current.activeTurnId === turnId) {
      await this.patchChat(chatId, { activeTurnId: null, updatedAt: new Date().toISOString() });
    }
  }

  /**
   * Record only a turn accepted by Runtime, before its events are delivered.
   * A later sender cannot overwrite that turn's origin. Records use the chat's
   * existing lifecycle and action retention bound; retired records fail closed.
   * This does not authenticate the platform SDK or pin replacement credentials.
   */
  async recordTurnOrigin(chatId, threadId, turnId, actorId) {
    const current = this.data.chats[chatId];
    if (!current || current.threadId !== threadId || !turnId || !actorId) {
      throw new ApprovalOwnershipError("accepted turn needs its current chat, thread and initiating human");
    }
    const origins = Array.isArray(current.turnOrigins) ? current.turnOrigins : [];
    const existing = origins.find((origin) => origin.threadId === threadId && origin.turnId === turnId);
    if (existing) {
      if (existing.actorId !== actorId) throw new ApprovalOwnershipError("turn origin cannot change");
      return existing;
    }
    const origin = { threadId, turnId, actorId };
    // patchChat mutates synchronously before saving, preserving concurrent turns.
    await this.patchChat(chatId, {
      turnOrigins: [...origins.filter((entry) => entry.threadId === threadId), origin].slice(-this.options.actionLimit)
    });
    return origin;
  }

  turnOrigin(chatId, threadId, turnId) {
    const state = this.data.chats[chatId];
    if (!state || state.threadId !== threadId || !turnId) return null;
    return (Array.isArray(state.turnOrigins) ? state.turnOrigins : [])
      .find((origin) => origin.threadId === threadId && origin.turnId === turnId && origin.actorId) || null;
  }

  async putAction(action, owner) {
    if (!this.options.actions) return "";
    this.ensureShape();
    const binding = actionOwner(owner);
    if (!binding.chatId) throw new ApprovalOwnershipError("button action needs its receiving chat");
    if (action.kind === "approval" &&
        (!binding.threadId || !binding.turnId || !binding.actorId ||
         this.turnOrigin(binding.chatId, binding.threadId, binding.turnId)?.actorId !== binding.actorId)) {
      throw new ApprovalOwnershipError("approval button needs the accepted turn and initiating human");
    }
    const token = randomBytes(16).toString("hex");
    this.data.actions[token] = {
      ...action,
      owner: binding,
      createdAt: new Date().toISOString()
    };
    this.pruneActions();
    await this.save();
    return token;
  }

  async getAction(token, owner = null) {
    if (!token || !this.options.actions) return null;
    this.ensureShape();
    this.pruneActions();
    const action = this.data.actions[token] || null;
    if (!action) return null;
    // Legacy unbound buttons cannot prove which chat received authority.
    if (!action.owner || !sameOwner(action.owner, owner)) return null;
    if (action.kind === "approval" &&
        (!action.owner.threadId || !action.owner.turnId || !action.owner.actorId ||
         this.turnOrigin(action.owner.chatId, action.owner.threadId, action.owner.turnId)?.actorId !== action.owner.actorId)) return null;
    return action;
  }

  async takeAction(token, owner = null) {
    const action = await this.getAction(token, owner);
    if (action) {
      delete this.data.actions[token];
      await this.save();
    }
    return action;
  }

  pruneActions() {
    if (!this.options.actions) return;
    const cutoff = Date.now() - this.options.actionTtlMs;
    const fresh = Object.entries(this.data.actions || {}).filter(([, action]) => {
      const time = Date.parse(action.createdAt || "");
      return Number.isFinite(time) && time >= cutoff;
    });
    this.data.actions = Object.fromEntries(fresh.slice(-this.options.actionLimit));
  }

  async save() {
    // Batch bursts of small updates: saves issued while a write is in flight
    // coalesce into a single follow-up write. The returned promise resolves
    // only after this mutation is durable on disk (temp file + rename).
    this.saveDirty = true;
    if (!this.savePending) {
      this.savePending = this.flushSaves();
    }
    return this.savePending;
  }

  async flushSaves() {
    try {
      while (this.saveDirty) {
        this.saveDirty = false;
        await this.writeSnapshot();
      }
    } finally {
      this.savePending = null;
    }
  }

  async writeSnapshot() {
    const dir = path.dirname(this.filePath);
    await mkdir(dir, { recursive: true, mode: 0o700 });
    if (this.options.privateMode) await chmodBestEffort(dir, 0o700);
    await writeFileDurable(this.filePath, `${JSON.stringify(this.data, null, 2)}\n`, { mode: 0o600 });
    if (this.options.privateMode) await chmodBestEffort(this.filePath, 0o600);
  }
}

function actionOwner(owner) {
  return {
    chatId: String(owner?.chatId ?? ""),
    ...(owner?.threadId ? { threadId: String(owner.threadId) } : {}),
    ...(owner?.turnId ? { turnId: String(owner.turnId) } : {}),
    ...(owner?.actorId ? { actorId: String(owner.actorId) } : {})
  };
}

function sameOwner(stored, owner) {
  if (!owner) return false;
  const wanted = actionOwner(owner);
  if (!stored.chatId || stored.chatId !== wanted.chatId) return false;
  return (!stored.threadId || stored.threadId === wanted.threadId) &&
    (!stored.actorId || stored.actorId === wanted.actorId) &&
    (!stored.turnId || !wanted.turnId || stored.turnId === wanted.turnId);
}

export class ApprovalOwnershipError extends Error {
  constructor(message) {
    super(message);
    this.name = "ApprovalOwnershipError";
  }
}

/**
 * Runtime owns pending approvals and their turn ids. The existing chat store
 * owns the immutable human who initiated that accepted turn. Admission to a
 * group, a newer sender or a legacy unbound button does not grant this authority.
 */
export async function decideApproval(runtimeJson, { store, chatId, actorId, approvalId, decision, remember = false, turnId = null }) {
  if (decision !== "allow" && decision !== "deny") throw new ApprovalOwnershipError("decision must be allow or deny");
  const state = await store.getChat(chatId);
  const threadId = state?.threadId;
  if (!threadId) throw new ApprovalOwnershipError("this chat has no Runtime thread yet");
  if (!actorId) throw new ApprovalOwnershipError("missing initiating human");
  if (!approvalId) throw new ApprovalOwnershipError("missing approval id");
  const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(threadId)}`);
  const pending = detail?.pending_approvals ?? detail?.thread?.pending_approvals ?? [];
  const approval = pending.find((entry) => (entry?.id ?? entry?.approval_id) === approvalId);
  const pendingTurnId = approval?.turn_id;
  if (!pendingTurnId || (turnId && pendingTurnId !== turnId) ||
      store.turnOrigin(chatId, threadId, pendingTurnId)?.actorId !== actorId) {
    throw new ApprovalOwnershipError("approval needs its accepted turn's initiating human in this chat");
  }
  await runtimeJson(`/v1/approvals/${encodeURIComponent(approvalId)}`, {
    method: "POST",
    body: { decision, remember: remember === true }
  });
}

/**
 * Settle a stored approval button: look it up for this owner, deliver the
 * decision, and consume the token only once the Runtime accepted it. A failed
 * POST leaves the button usable.
 */
export async function settleStoredApproval(store, token, owner, deliver) {
  const stored = await store.getAction(token, owner);
  if (!stored || stored.kind !== "approval") return null;
  await deliver(stored);
  await store.takeAction(token, owner);
  return stored;
}

export function envFirst(env, ...names) {
  for (const name of names) {
    const value = env?.[name];
    if (value != null && String(value).trim()) return String(value).trim();
  }
  return "";
}

export function parseList(raw) {
  return String(raw || "")
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

export function parseBool(raw, fallback = false) {
  if (raw == null || raw === "") return fallback;
  return ["1", "true", "yes", "on"].includes(String(raw).trim().toLowerCase());
}

export function parseEnvText(raw) {
  const env = {};
  for (const line of String(raw || "").split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const normalized = trimmed.startsWith("export ") ? trimmed.slice(7).trim() : trimmed;
    const index = normalized.indexOf("=");
    if (index <= 0) continue;
    const key = normalized.slice(0, index).trim();
    let value = normalized.slice(index + 1).trim();
    if (
      value.length >= 2 &&
      ((value.startsWith('"') && value.endsWith('"')) ||
        (value.startsWith("'") && value.endsWith("'")))
    ) {
      value = value.slice(1, -1);
    }
    env[key] = value;
  }
  return env;
}

export function cleanEnvValue(value) {
  return String(value ?? "").trim();
}

export function isPlaceholderValue(value) {
  const normalized = cleanEnvValue(value).toLowerCase();
  return (
    !normalized ||
    normalized.includes("replace-with") ||
    normalized.includes("xxxxxxxx") ||
    normalized === "changeme"
  );
}

export function parseTextContent(content, keys = ["text", "content"]) {
  if (typeof content !== "string") return "";
  try {
    const parsed = JSON.parse(content);
    for (const key of keys) {
      if (typeof parsed?.[key] === "string") return parsed[key];
    }
  } catch {
    return content;
  }
  return content;
}

export function stripGroupPrefix(text, { chatType, requirePrefix, prefix, directChatTypes = [] }) {
  const trimmed = String(text || "").trim();
  if (!trimmed) return { accepted: false, text: "" };
  if (!requirePrefix || directChatTypes.includes(chatType)) {
    return { accepted: true, text: trimmed };
  }
  const marker = prefix || "/ds";
  if (trimmed === marker) return { accepted: true, text: "/help" };
  if (trimmed.startsWith(`${marker} `)) {
    return { accepted: true, text: trimmed.slice(marker.length).trim() };
  }
  return { accepted: false, text: "" };
}

export function parseCommand(text, options = {}) {
  const trimmed = String(text || "").trim();
  if (!trimmed.startsWith("/")) return { name: "prompt", args: trimmed };
  const [head, ...rest] = trimmed.split(/\s+/);
  const rawName = head.slice(1);
  const name = (options.stripBotMention ? rawName.split("@")[0] : rawName).toLowerCase();
  return {
    name,
    args: rest.join(" ").trim()
  };
}

export function parseApprovalDecisionArgs(args) {
  const parts = String(args || "")
    .split(/\s+/)
    .filter(Boolean);
  return {
    approvalId: parts[0] || "",
    remember: parts.slice(1).includes("remember")
  };
}

export function commandAction(command, options = {}) {
  const allowMenu = options.allowMenu === true;
  const allowStart = options.allowStart === true;
  switch (command.name) {
    case "start":
      if (allowStart) return { kind: "help" };
      break;
    case "help":
      return { kind: "help" };
    case "menu":
      if (allowMenu) return { kind: "menu" };
      break;
    case "status":
      return { kind: "status" };
    case "threads":
      return { kind: "threads" };
    case "new":
      return { kind: "new_thread" };
    case "resume":
      return { kind: "resume", threadId: command.args };
    case "interrupt":
      return { kind: "interrupt" };
    case "compact":
      return { kind: "compact" };
    case "model":
      return { kind: "set_model", modelName: command.args };
    case "allow":
      return { kind: "approval", decision: "allow", ...parseApprovalDecisionArgs(command.args) };
    case "deny":
      return { kind: "approval", decision: "deny", ...parseApprovalDecisionArgs(command.args) };
    case "prompt":
      return { kind: "prompt", prompt: command.args };
    default:
      break;
  }
  return {
    kind: "prompt",
    prompt: `/${command.name}${command.args ? ` ${command.args}` : ""}`
  };
}

export function preservedChatStateFields(state = {}, fields = ["model", "authorizedIdentity"]) {
  const preserved = {};
  for (const field of fields) {
    if (Object.prototype.hasOwnProperty.call(state || {}, field)) {
      preserved[field] = state[field] || null;
    }
  }
  return preserved;
}

export function splitMessage(text, maxChars = 3500) {
  const value = String(text || "");
  const limit = Math.max(1, Math.floor(Number(maxChars) || 3500));
  // Materialize code points once; all chunking below works on index ranges
  // instead of re-running Array.from over the shrinking remainder.
  const chars = Array.from(value);
  if (chars.length <= limit) return value ? [value] : [];
  const chunks = [];
  let offset = 0;
  let openFence = null;
  while (offset < chars.length) {
    const next = takeRenderedSplitMessageChunk(chars, offset, limit, openFence);
    chunks.push(next.chunk);
    offset = next.offset;
    openFence = next.openFence;
  }
  return chunks;
}

function takeRenderedSplitMessageChunk(chars, offset, maxChars, openFence) {
  const prefix = openFence !== null ? `\`\`\`${openFence}\n` : "";
  const prefixLength = charLength(prefix);
  let payloadLimit = Math.max(1, maxChars - prefixLength);

  while (true) {
    const splitAt = splitMessageChunkEnd(chars, offset, payloadLimit);
    const payload = chars.slice(offset, splitAt).join("");
    const body = `${prefix}${payload}`;
    const nextOpenFence = updateCodeFenceState(openFence, payload);
    const suffix =
      nextOpenFence !== null && splitAt < chars.length ? (body.endsWith("\n") ? "```" : "\n```") : "";
    // prefix/suffix are ASCII fence markup, so string length == code points.
    const overflow = prefixLength + (splitAt - offset) + suffix.length - maxChars;
    if (overflow <= 0 || payloadLimit === 1) {
      return { chunk: `${body}${suffix}`, offset: splitAt, openFence: nextOpenFence };
    }
    payloadLimit = Math.max(1, payloadLimit - overflow);
  }
}

function splitMessageChunkEnd(chars, offset, maxChars) {
  if (chars.length - offset <= maxChars) return chars.length;
  return offset + preferredSplitIndex(chars, offset, maxChars);
}

function preferredSplitIndex(chars, offset, maxChars) {
  const limit = Math.min(chars.length - offset, maxChars);
  for (let i = limit - 1; i > 0; i -= 1) {
    if (chars[offset + i] === "\n") return i + 1;
  }
  for (let i = limit - 1; i > 0; i -= 1) {
    if (/\s/u.test(chars[offset + i])) return i + 1;
  }
  return limit;
}

function charLength(text) {
  let length = 0;
  for (const _ of text) length += 1;
  return length;
}

function updateCodeFenceState(openFence, text) {
  let current = openFence;
  for (const match of text.matchAll(/^```([^\n`]*)\s*$/gm)) {
    if (current === null) {
      current = match[1]?.trim() || "";
    } else {
      current = null;
    }
  }
  return current;
}

export async function readJsonSafe(response, { jsonOnly = false } = {}) {
  const limit = response.ok ? 8 * 1024 * 1024 : 4096;
  const oversized = () => {
    if (response.ok) throw new Error("Runtime response exceeds the size limit");
    return jsonOnly ? null : "Runtime error body exceeds the size limit";
  };
  if (Number(response.headers.get("content-length")) > limit) {
    await response.body?.cancel().catch(() => {});
    return oversized();
  }
  const decoder = new TextDecoder();
  let size = 0;
  let text = "";
  if (response.body) {
    for await (const chunk of response.body) {
      size += chunk.byteLength;
      if (size > limit) return oversized();
      text += decoder.decode(chunk, { stream: true });
    }
    text += decoder.decode();
  }
  if (!text) return jsonOnly ? null : {};
  try {
    return JSON.parse(text);
  } catch {
    return jsonOnly ? null : text;
  }
}

const MAX_EVENT_BYTES = 2 * 1024 * 1024;

export async function* readSse(response) {
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let line = Buffer.allocUnsafe(1024);
  let length = 0, frameBytes = 0, afterCr = false;
  let event = "", data = "", sawData = false;

  for await (const chunk of response.body) {
    for (const byte of chunk) {
      const pairedLf = afterCr && byte === 10;
      afterCr = byte === 13;
      if (pairedLf && frameBytes === 0) continue;
      if (++frameBytes > MAX_EVENT_BYTES) throw new Error("Runtime event frame exceeds the size limit");
      if (pairedLf) continue;
      if (byte !== 10 && byte !== 13) {
        if (length === line.length) {
          const larger = Buffer.allocUnsafe(Math.min(MAX_EVENT_BYTES, line.length * 2));
          line.copy(larger);
          line = larger;
        }
        line[length++] = byte;
        continue;
      }
      if (length === 0) {
        if (sawData || event) yield { event, data };
        event = "";
        data = "";
        sawData = false;
        frameBytes = 0;
        continue;
      }
      const field = decoder.decode(line.subarray(0, length));
      length = 0;
      const colon = field.indexOf(":");
      const name = colon < 0 ? field : field.slice(0, colon);
      const value = colon < 0 ? "" : field.slice(colon + (field[colon + 1] === " " ? 2 : 1));
      if (name === "event") event = value;
      if (name === "data") {
        data += (sawData ? "\n" : "") + value;
        sawData = true;
      }
    }
  }
  if (frameBytes) throw new Error("Runtime event stream ended inside a frame");
}

export async function* readThreadEvents(response, threadId) {
  if (!threadId) throw new Error("Missing expected Runtime thread identity");
  for await (const { data } of readSse(response)) {
    if (!data) continue;
    const record = JSON.parse(data);
    if (record?.thread_id !== threadId || typeof record.event !== "string" || !record.event) {
      throw new Error("Invalid Runtime event or thread identity");
    }
    if (record.event === "stream.end") {
      yield record;
      return;
    }
    if (!Number.isSafeInteger(record.seq) || record.seq < 0) throw new Error("Invalid Runtime event sequence");
    yield record;
  }
  throw new Error("Runtime event stream ended before a terminal event");
}

export function createRuntimeClient({ runtimeUrl, runtimeToken, runtimeRequestTimeoutMs }) {
  function authHeaders() {
    return { authorization: `Bearer ${runtimeToken}` };
  }

  async function runtimeJson(route, options = {}) {
    const response = await fetch(`${runtimeUrl}${route}`, {
      method: options.method || "GET",
      headers: {
        ...(options.auth === false ? {} : authHeaders()),
        ...(options.body ? { "content-type": "application/json" } : {})
      },
      body: options.body ? JSON.stringify(options.body) : undefined,
      signal: options.signal || (runtimeRequestTimeoutMs ? AbortSignal.timeout(runtimeRequestTimeoutMs) : undefined)
    });
    const body = await readJsonSafe(response);
    if (!response.ok) {
      const error = new Error(compactRuntimeError(response.status, body));
      error.status = response.status;
      throw error;
    }
    return body;
  }

  return { runtimeJson, authHeaders };
}

export function compactRuntimeError(status, body) {
  const message =
    body?.error?.message ||
    body?.message ||
    (typeof body === "string" ? body : JSON.stringify(body));
  return `Runtime API request failed (${status}): ${message}`;
}

export function questionText(request) {
  const questions = Array.isArray(request?.questions) ? request.questions : [];
  const lines = ["需要你回答 / Codewhale is asking"];
  for (const [index, question] of questions.entries()) {
    const prefix = questions.length > 1 ? `${index + 1}. ` : "";
    lines.push(`${prefix}${question.header ? `${question.header}: ` : ""}${question.question || ""}`);
    for (const [optionIndex, option] of (question.options || []).entries()) {
      const description = String(option.description || "").trim();
      lines.push(`  ${optionIndex + 1}) ${option.label}${option.description ? ` - ${description.length > 120 ? `${description.slice(0, 119)}…` : description}` : ""}`);
    }
  }
  lines.push(questions.length > 1
    ? "每行回答一个问题，可回复选项编号或文字。/ Answer one question per line, by option number or in words."
    : "回复选项编号或文字。/ Reply with an option number or in words.");
  return lines.join("\n");
}

export function parseQuestionAnswers(request, text) {
  const questions = Array.isArray(request?.questions) ? request.questions : [];
  if (!questions.length) return null;
  const replies = questions.length > 1
    ? String(text || "").split(/\n+/).map((line) => line.trim()).filter(Boolean)
    : [String(text || "").trim()];
  if (replies.length !== questions.length || replies.some((reply) => !reply)) return null;
  const answers = [];
  for (const [index, question] of questions.entries()) {
    const reply = replies[index];
    const options = question.options || [];
    const picks = question.multi_select ? reply.split(/[\s,，、]+/u).filter(Boolean) : [reply];
    const chosen = picks.every((pick) => /^\d+$/.test(pick) && Number(pick) >= 1 && Number(pick) <= options.length)
      ? picks.map((pick) => options[Number(pick) - 1])
      : options.filter((option) => option.label.toLowerCase() === reply.toLowerCase());
    if (chosen.length) {
      for (const option of chosen) answers.push({ id: question.id, label: option.label, value: option.label });
    } else if (question.allow_free_text || !options.length) {
      answers.push({ id: question.id, label: "Other", value: reply });
    } else {
      return null;
    }
  }
  return answers;
}

export async function admitTurn({ runtimeJson, pending, capabilities, persistPending, ensureThread }) {
  if (!pending?.request || (pending.operationKey && pending.request.operation_key !== pending.operationKey)) {
    throw new Error("Invalid retained turn admission");
  }
  if (pending.operationKey && (capabilities?.turn_operation_idempotency !== true || capabilities?.turn_operation_lookup !== true)) {
    return { pending, turn: null, uncertain: true };
  }
  if (!pending.threadId) {
    const threadId = await ensureThread(pending.threadRequest);
    if (!threadId) throw new Error("Turn admission requires a durable thread binding");
    pending = { ...pending, threadId };
    await persistPending(pending);
  }
  if (pending.sinceSeq === null) {
    const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(pending.threadId)}`);
    if (latestRunningTurn(detail)) return { pending, turn: null, blocked: true };
    pending = { ...pending, sinceSeq: Number(detail.latest_seq || 0) };
    await persistPending(pending);
  }
  let turn;
  if (pending.submitted) {
    if (!pending.operationKey || capabilities?.turn_operation_idempotency !== true || capabilities?.turn_operation_lookup !== true) {
      return { pending, turn: null, uncertain: true };
    }
    try {
      turn = await runtimeJson(`/v1/threads/${encodeURIComponent(pending.threadId)}/turn-operations/${encodeURIComponent(pending.operationKey)}`);
    } catch (error) {
      if (error.status !== 404) throw error;
      const detail = await runtimeJson(`/v1/threads/${encodeURIComponent(pending.threadId)}`);
      if (latestRunningTurn(detail)) return { pending, turn: null, blocked: true };
    }
  }
  if (!turn) {
    pending = { ...pending, submitted: true };
    await persistPending(pending);
    const result = await runtimeJson(`/v1/threads/${encodeURIComponent(pending.threadId)}/turns`, {
      method: "POST", body: pending.request
    });
    turn = result?.turn;
  }
  if (!turn?.id || turn.thread_id !== pending.threadId) throw new Error("Invalid Engine admission receipt");
  return { pending, turn };
}

export function latestRunningTurn(detail) {
  const turns = Array.isArray(detail?.turns) ? detail.turns : [];
  for (let index = turns.length - 1; index >= 0; index -= 1) {
    const turn = turns[index];
    if (["queued", "in_progress"].includes(turn?.status)) return turn;
  }
  return null;
}

export function activeTurnBlock(detail, state = {}) {
  const runningTurn = latestRunningTurn(detail);
  if (!runningTurn) return null;
  const activeTurnId = state?.activeTurnId || "";
  return {
    turnId: runningTurn.id || activeTurnId,
    message: `Thread already has active turn ${
      runningTurn.id || activeTurnId || "(unknown)"
    }. Wait for it to finish or send /interrupt.`
  };
}
