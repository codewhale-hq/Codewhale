export const STREAM_EVENT_NAMES = [
  "thread.started",
  "thread.updated",
  "thread.forked",
  "thread.permissions_changed",
  "turn.started",
  "turn.lifecycle",
  "turn.usage",
  "turn.steered",
  "turn.steer_dropped",
  "turn.interrupt_requested",
  "turn.artifacts",
  "turn.workspace_snapshot",
  "turn.completed",
  "item.started",
  "item.delta",
  "item.completed",
  "item.canceled",
  "item.failed",
  "item.interrupted",
  "approval.required",
  "approval.decided",
  "approval.timeout",
  "approval.grant_added",
  "approval.grant_revoked",
  "user_input.required",
  "user_input.answered",
  "user_input.canceled",
  "sandbox.denied",
  "agent.spawned",
  "agent.progress",
  "agent.completed",
  "agent.list",
  "tool_call.requested",
  "tool_call.resolved",
  "tool_call.canceled",
  "tool_call.timeout",
];

export function createThreadState(threadId = "") {
  return {
    threadId,
    thread: null,
    turns: new Map(),
    turnOrder: [],
    items: new Map(),
    itemOrder: [],
    latestSeq: 0,
    approvals: new Map(),
    userInputs: new Map(),
    dynamicToolCalls: new Map(),
  };
}

export function applySnapshot(state, detail, expectedThreadId = state.threadId) {
  if (!detail || !detail.thread || detail.thread.id !== expectedThreadId) {
    return false;
  }
  state.threadId = expectedThreadId;
  state.thread = detail.thread;
  state.turns = new Map();
  state.turnOrder = [];
  for (const turn of Array.isArray(detail.turns) ? detail.turns : []) {
    if (!turn || !turn.id) continue;
    state.turns.set(turn.id, turn);
    state.turnOrder.push(turn.id);
  }
  state.items = new Map();
  state.itemOrder = [];
  for (const item of Array.isArray(detail.items) ? detail.items : []) {
    if (!item || !item.id) continue;
    state.items.set(item.id, item);
    state.itemOrder.push(item.id);
  }
  state.latestSeq = normalizedSequence(detail.latest_seq);
  state.approvals = new Map();
  for (const approval of Array.isArray(detail.pending_approvals) ? detail.pending_approvals : []) {
    const approvalId = approval?.approval_id || approval?.id;
    if (approvalId) state.approvals.set(approvalId, approval);
  }
  state.userInputs = new Map();
  for (const input of Array.isArray(detail.pending_user_inputs) ? detail.pending_user_inputs : []) {
    const inputId = input?.input_id || input?.id;
    if (inputId) state.userInputs.set(inputId, input);
  }
  state.dynamicToolCalls = new Map();
  for (const call of Array.isArray(detail.pending_dynamic_tool_calls) ? detail.pending_dynamic_tool_calls : []) {
    if (call?.call_id) state.dynamicToolCalls.set(call.call_id, call);
  }
  return true;
}

export function applyRuntimeEvent(state, envelope) {
  if (runtimeEventContinuity(state, envelope) !== "next") {
    return false;
  }
  const sequence = normalizedSequence(envelope.seq);
  state.latestSeq = sequence;

  const eventName = envelope.event || envelope.kind || "";
  const payload = envelope.payload && typeof envelope.payload === "object"
    ? envelope.payload
    : {};

  if (
    (eventName === "thread.started" || eventName === "thread.updated" || eventName === "thread.forked")
    && payload.thread
  ) {
    state.thread = payload.thread;
  } else if (eventName === "turn.started" || eventName === "turn.completed") {
    if (payload.turn) upsertTurn(state, payload.turn);
    if (eventName === "turn.completed") {
      clearTurnAttention(state, envelope.turn_id || payload.turn?.id || "");
    }
  } else if (eventName === "turn.lifecycle") {
    const turnId = envelope.turn_id;
    const turn = turnId ? state.turns.get(turnId) : null;
    if (turn && payload.status) {
      state.turns.set(turnId, { ...turn, status: payload.status });
    }
  } else if (eventName === "turn.interrupt_requested") {
    const turnId = envelope.turn_id;
    const turn = turnId ? state.turns.get(turnId) : null;
    if (turn) state.turns.set(turnId, { ...turn, status: "in_progress" });
  } else if (eventName === "turn.workspace_snapshot") {
    // The payload is one restore-point receipt; the turn record keeps them
    // in the order the engine took them (`workspace_snapshots`).
    const turnId = envelope.turn_id;
    const turn = turnId ? state.turns.get(turnId) : null;
    if (turn && payload.kind && payload.tree_id) {
      const receipts = Array.isArray(turn.workspace_snapshots) ? turn.workspace_snapshots : [];
      state.turns.set(turnId, { ...turn, workspace_snapshots: [...receipts, payload] });
    }
  } else if (
    eventName === "item.started"
    || eventName === "item.completed"
    || eventName === "item.canceled"
    || eventName === "item.failed"
    || eventName === "item.interrupted"
    || eventName === "agent.spawned"
    || eventName === "agent.progress"
    || eventName === "agent.completed"
    || eventName === "agent.list"
  ) {
    if (payload.item) upsertItem(state, payload.item);
  } else if (eventName === "item.delta") {
    appendItemDelta(state, envelope.item_id, payload);
  } else if (eventName === "approval.required") {
    const approvalId = payload.approval_id || payload.id;
    if (approvalId) {
      state.approvals.set(approvalId, {
        ...payload,
        turn_id: payload.turn_id || envelope.turn_id || "",
      });
    }
  } else if (eventName === "approval.decided" || eventName === "approval.timeout") {
    const approvalId = payload.approval_id || payload.id;
    if (approvalId) state.approvals.delete(approvalId);
  } else if (eventName === "user_input.required") {
    const inputId = payload.id;
    if (inputId) {
      state.userInputs.set(inputId, {
        ...payload,
        turn_id: payload.turn_id || envelope.turn_id || "",
      });
    }
  } else if (eventName === "user_input.answered" || eventName === "user_input.canceled") {
    const inputId = payload.input_id || payload.id;
    if (inputId) state.userInputs.delete(inputId);
  } else if (eventName === "tool_call.requested") {
    if (payload.call_id) {
      state.dynamicToolCalls.set(payload.call_id, {
        ...payload,
        turn_id: payload.turn_id || envelope.turn_id || "",
      });
    }
  } else if (
    eventName === "tool_call.resolved"
    || eventName === "tool_call.canceled"
    || eventName === "tool_call.timeout"
  ) {
    if (payload.call_id) state.dynamicToolCalls.delete(payload.call_id);
  }
  return true;
}

function clearTurnAttention(state, turnId) {
  for (const [id, approval] of state.approvals) {
    if (!approval?.turn_id || approval.turn_id === turnId) state.approvals.delete(id);
  }
  for (const [id, input] of state.userInputs) {
    if (!input?.turn_id || input.turn_id === turnId) state.userInputs.delete(id);
  }
  for (const [id, call] of state.dynamicToolCalls) {
    if (!call?.turn_id || call.turn_id === turnId) state.dynamicToolCalls.delete(id);
  }
}

export function runtimeEventContinuity(state, envelope) {
  if (!envelope || envelope.thread_id !== state.threadId) {
    return "ignore";
  }
  const sequence = normalizedSequence(envelope.seq);
  if (sequence <= state.latestSeq) {
    return "ignore";
  }
  if (Object.hasOwn(envelope, "previous_seq")) {
    const previousSequence = normalizedSequence(envelope.previous_seq);
    if (previousSequence !== state.latestSeq) {
      return "gap";
    }
  }
  return "next";
}

export async function snapshotThenSubscribe({
  state,
  threadId,
  loadSnapshot,
  subscribe,
  isCurrent = () => true,
}) {
  const detail = await loadSnapshot(threadId);
  if (!isCurrent() || !applySnapshot(state, detail, threadId)) {
    return false;
  }
  if (!isCurrent()) return false;
  // A recovery caller may return a stream-open handshake. Await it so a
  // replacement snapshot is not called continuous until the replacement SSE
  // stream has actually opened. Synchronous subscribers remain supported.
  await subscribe(threadId, state.latestSeq);
  return true;
}

export async function recoverSnapshotAndSubscribe(options, onRecovered) {
  const subscribed = await snapshotThenSubscribe(options);
  if (!subscribed) return false;
  onRecovered();
  return true;
}

// ---------------------------------------------------------------------------
// Typed selection identity (#4397)
//
// The dashboard can have two very different things selected: a *saved session*
// (a recording on disk) or a *live thread* (a running runtime object). Almost
// every safety rule in this slice reduces to "which one is it?", so the answer
// is a typed value rather than a pair of loosely-related string fields that can
// both be set, both be empty, or disagree.
// ---------------------------------------------------------------------------

// Nothing selected.
export const NO_TARGET = Object.freeze({ kind: "none" });

// A saved session: read-only. Peek only, never reply, never approve.
export function sessionTarget(sessionId) {
  return Object.freeze({ kind: "session", sessionId: String(sessionId || "") });
}

// A live thread: the only thing that can receive a reply or an approval.
export function threadTarget(threadId) {
  return Object.freeze({ kind: "thread", threadId: String(threadId || "") });
}

// May the composer send to this target?
//
// Only a live thread. A saved session has no runtime to receive a message;
// offering a composer against one would be an affordance with nothing behind
// it, and "resume it silently on send" would attach the user's message to a
// thread they never asked to create.
export function canReply(target) {
  return target?.kind === "thread" && Boolean(target.threadId);
}

// Resolve the id a reply must be POSTed to, or an explicit refusal.
//
// Fails closed on every ambiguity: no target, a session target, or a target
// whose thread is not the one the live stream is following (a stale target —
// the user changed rows while a request was in flight).
export function resolveReplyTarget(target, streamState) {
  if (!target || target.kind === "none") {
    return { ok: false, reason: "no-target" };
  }
  if (target.kind === "session") {
    return { ok: false, reason: "session-not-live" };
  }
  if (!target.threadId) {
    return { ok: false, reason: "no-target" };
  }
  if (streamState && streamState.threadId && streamState.threadId !== target.threadId) {
    return { ok: false, reason: "stale-target" };
  }
  return { ok: true, threadId: target.threadId };
}

// Resolve an approval decision to the thread that owns it, or refuse.
//
// An approval is authority: answering the wrong one, or answering one that
// has already been decided elsewhere, is worse than not answering. So the
// approval must be present in the *current* stream state, and that state must
// belong to the selected live thread.
export function resolveApprovalTarget(approvalId, target, streamState) {
  const reply = resolveReplyTarget(target, streamState);
  if (!reply.ok) return reply;
  if (!approvalId) return { ok: false, reason: "no-approval" };
  if (!streamState || streamState.threadId !== reply.threadId) {
    return { ok: false, reason: "stale-target" };
  }
  if (!streamState.approvals || !streamState.approvals.has(approvalId)) {
    // Decided, timed out, or belonging to a thread we are no longer watching.
    return { ok: false, reason: "stale-approval" };
  }
  return { ok: true, threadId: reply.threadId, approvalId };
}

// Resolve a user-input submission to the live thread and pending request that
// own it. User-input answers can resume a paused turn, so a stale card must not
// be able to answer a request from another thread or one already settled by a
// different client.
export function resolveUserInputTarget(inputId, target, streamState) {
  const reply = resolveReplyTarget(target, streamState);
  if (!reply.ok) return reply;
  if (!inputId) return { ok: false, reason: "no-user-input" };
  if (!streamState || streamState.threadId !== reply.threadId) {
    return { ok: false, reason: "stale-target" };
  }
  if (!streamState.userInputs || !streamState.userInputs.has(inputId)) {
    return { ok: false, reason: "stale-user-input" };
  }
  return { ok: true, threadId: reply.threadId, inputId };
}

function ownAnswerValue(collection, id) {
  if (collection instanceof Map) return collection.get(id);
  if (collection && typeof collection === "object" && Object.hasOwn(collection, id)) {
    return collection[id];
  }
  return undefined;
}

// Build the exact Runtime answer payload from selected options and custom
// text. This keeps single-select questions single, rejects stale/forged option
// values, and preserves the TUI rule that a custom answer is always reachable.
export function answersForUserInput(request, selections = {}, freeText = {}) {
  const questions = Array.isArray(request?.questions) ? request.questions : [];
  if (questions.length === 0) {
    return { ok: false, reason: "invalid-request", question: "the question" };
  }

  const answers = [];
  const questionIds = new Set();
  for (const question of questions) {
    const id = String(question?.id || "");
    const questionLabel = String(question?.header || question?.question || id || "the question");
    if (!id || questionIds.has(id)) {
      return { ok: false, reason: "invalid-request", question: questionLabel };
    }
    questionIds.add(id);

    const optionLabels = new Set(
      (Array.isArray(question.options) ? question.options : [])
        .map((option) => String(option?.label || ""))
        .filter((value) => Boolean(value.trim())),
    );
    const selectedValue = ownAnswerValue(selections, id);
    const selected = Array.isArray(selectedValue)
      ? [...new Set(selectedValue.map((value) => String(value)).filter((value) => value.trim()))]
      : [];
    if (selected.some((value) => !optionLabels.has(value))) {
      return { ok: false, reason: "invalid-option", question: questionLabel };
    }

    const otherValue = ownAnswerValue(freeText, id);
    const other = String(otherValue || "").trim();
    const count = selected.length + (other ? 1 : 0);
    if (count === 0) {
      return { ok: false, reason: "missing-answer", question: questionLabel };
    }
    if (!question.multi_select && count !== 1) {
      return { ok: false, reason: "multiple-answers", question: questionLabel };
    }

    for (const value of selected) answers.push({ id, label: value, value });
    if (other) answers.push({ id, label: "Other", value: other });
  }
  return { ok: true, answers };
}

// Human-readable reason for a refusal, for the status banner.
export function refusalMessage(reason) {
  switch (reason) {
    case "session-not-live":
      return "This is a saved session, not a live thread — nothing was sent. Resume it first to reply.";
    case "stale-target":
      return "That thread is no longer the selected one — nothing was sent.";
    case "stale-approval":
      return "That request was already answered or has expired — nothing was sent.";
    case "no-approval":
      return "No approval was identified — nothing was sent.";
    case "stale-user-input":
      return "That question was already answered or has expired — nothing was sent.";
    case "no-user-input":
      return "No user-input request was identified — nothing was sent.";
    default:
      return "Select a live thread first — nothing was sent.";
  }
}

// The SSE resume cursor and whether the stream is known to have a gap.
//
// Surfaced rather than kept internal: after a reconnect the user needs to
// know whether what they are reading is continuous or whether events were
// missed and a re-snapshot is pending.
export function streamCursor(state, { gap = false, connected = true } = {}) {
  const seq = normalizedSequence(state?.latestSeq);
  return {
    latestSeq: seq,
    gap: Boolean(gap),
    connected: Boolean(connected),
    label: !connected
      ? `Reconnecting — resuming from #${seq}`
      : gap
        ? `Gap detected — re-syncing from #${seq}`
        : `Live — event #${seq}`,
  };
}

// Keep machine diagnostics available without making them the conversation's
// loudest content. Known transport failures get one calm product sentence;
// the byte-for-byte receipt remains behind the disclosure.
export function receiptPresentation(item = {}) {
  const detail = String(item.detail || item.summary || "");
  const raw = String(item.summary || detail || humanize(item.kind));
  const fullRaw = detail && detail !== raw ? `${raw}\n\n${detail}` : raw;
  const workflow = workflowReceiptPresentation(item, detail, fullRaw);
  if (workflow) return workflow;
  const mcpFailure = raw.match(/Failed to connect MCP server ['"]?([^'":\s]+)['"]?/i);
  if (mcpFailure) {
    const server = mcpFailure[1] || "server";
    return {
      label: "MCP · Unavailable",
      summary: `${server} could not connect`,
      raw,
      failed: true,
    };
  }
  const failed = item.status === "failed" || /^(?:error|failed|failure)\b/i.test(raw);
  return {
    label: `${humanize(item.kind)} · ${humanize(item.status)}`,
    summary: raw,
    raw: fullRaw,
    failed,
  };
}

export function workflowReceiptPresentation(item, detail, raw) {
  const metadata = item?.metadata && typeof item.metadata === "object"
    ? item.metadata
    : {};
  let payload = null;
  try {
    const parsed = JSON.parse(detail);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) payload = parsed;
  } catch (_error) {
    // Non-JSON tool receipts continue through the ordinary presentation path.
  }
  const looksLikeWorkflow = /^workflow(?:\s|:)/i.test(String(item?.summary || ""))
    || Object.hasOwn(metadata, "dispatch_failure_count")
    || Boolean(payload && Object.hasOwn(payload, "dispatch_failure_count"));
  if (!looksLikeWorkflow) return null;

  const status = String(metadata.status || payload?.status || "").toLowerCase();
  const countValue = metadata.dispatch_failure_count ?? payload?.dispatch_failure_count;
  const count = Number(countValue);
  const rejected = Number.isSafeInteger(count) && count > 0 ? count : 0;
  if (rejected === 0 && status !== "degraded" && status !== "failed") return null;

  const summary = rejected === 1
    ? "1 task dispatch was rejected"
    : rejected > 1
      ? `${rejected} task dispatches were rejected`
      : status === "failed"
        ? "The workflow did not complete"
        : "The workflow completed with degraded results";
  return {
    label: status === "failed" ? "Workflow · Failed" : "Workflow · Needs attention",
    summary,
    raw,
    failed: true,
  };
}

export function eventStreamUrl(threadId, latestSeq, ticket = "") {
  const url = `/v1/threads/${encodeURIComponent(threadId)}/events?since_seq=${normalizedSequence(latestSeq)}`;
  return ticket ? `${url}&web_stream_ticket=${encodeURIComponent(ticket)}` : url;
}

export function createWebSessionFetch({ location, history, storage, fetch, pageProof = "" }) {
  const key = "codewhale_web_request_proof";
  const fragmentProof = new URLSearchParams(location.hash.slice(1)).get("p");
  if (fragmentProof) history.replaceState(null, "", location.pathname + location.search);
  let proof = pageProof || fragmentProof || "";
  if (proof) {
    try { storage().setItem(key, proof); } catch (_) { /* Memory-only session. */ }
  } else {
    try { proof = storage().getItem(key) || ""; } catch (_) { /* Storage unavailable. */ }
  }
  return (path, options = {}) => {
    const headers = new Headers(options.headers || {});
    headers.set("x-codewhale-web-request", proof);
    if (options.body != null && !headers.has("content-type")) {
      headers.set("content-type", "application/json");
    }
    return fetch(path, { ...options, headers, credentials: "same-origin", cache: "no-store" });
  };
}

// One owner for pending ticket requests, EventSources, and reconnect timers.
// Ticket HTTP 401/403 ends retries; EventSource hides HTTP status, so its
// errors retry through a fresh ticket request. Snapshot recovery owns its
// own retry when it requests an open handshake.
export function createStreamConnector({
  app, api, EventSource, receive, setConnection, showStatus,
  setTimeout = globalThis.setTimeout, clearTimeout = globalThis.clearTimeout,
}) {
  let attempt = 0;
  let retryDelay = 900;
  function stopStream() {
    attempt += 1;
    if (app.streamOpenCancel) app.streamOpenCancel();
    app.streamOpenCancel = null;
    if (app.stream) app.stream.close();
    app.stream = null;
    if (app.reconnectTimer) clearTimeout(app.reconnectTimer);
    app.reconnectTimer = null;
  }

  async function connectStream(threadId, sequence, generation, waitForOpen = false) {
    if (generation !== app.generation || threadId !== app.selectedThreadId) return;
    stopStream();
    const currentAttempt = attempt;
    const isCurrent = () => currentAttempt === attempt
      && generation === app.generation && threadId === app.selectedThreadId;
    const reconnect = () => {
      if (!isCurrent() || app.stream !== null) return;
      setConnection("", "Reconnecting to local runtime…");
      app.reconnectTimer = setTimeout(() => {
        app.reconnectTimer = null;
        if (!isCurrent() || app.stream !== null) return;
        void connectStream(threadId, app.threadState.latestSeq, generation).catch(() => {});
      }, retryDelay);
      retryDelay = Math.min(retryDelay * 2, 30_000);
    };
    let ticket;
    try {
      ({ stream_ticket: ticket } = await api("/__codewhale/web/stream-ticket", { method: "POST" }));
    } catch (error) {
      if (!isCurrent()) return;
      showStatus(error.message);
      setConnection("error", "Runtime connection failed");
      if (!waitForOpen && error.status !== 401 && error.status !== 403) reconnect();
      throw error;
    }
    if (!isCurrent() || app.stream !== null) return;
    const stream = new EventSource(eventStreamUrl(threadId, sequence, ticket), { withCredentials: true });
    app.stream = stream;
    let opened = false;
    let resolveOpen;
    let rejectOpen;
    const openHandshake = waitForOpen
      ? new Promise((resolve, reject) => {
          resolveOpen = resolve;
          rejectOpen = reject;
        })
      : undefined;
    const cancelOpen = () => {
      if (!rejectOpen) return;
      const reject = rejectOpen;
      resolveOpen = null;
      rejectOpen = null;
      reject(new Error("Runtime event stream open was cancelled"));
    };
    if (waitForOpen) app.streamOpenCancel = cancelOpen;
    const clearOpenHandshake = () => {
      if (app.streamOpenCancel === cancelOpen) app.streamOpenCancel = null;
    };
    stream.onopen = () => {
      if (!isCurrent() || app.stream !== stream) {
        stream.close();
        return;
      }
      retryDelay = 900;
      opened = true;
      setConnection("ready", "Local runtime connected");
      clearOpenHandshake();
      if (resolveOpen) resolveOpen();
      resolveOpen = null;
      rejectOpen = null;
    };
    for (const name of STREAM_EVENT_NAMES) {
      stream.addEventListener(name, (message) => {
        if (isCurrent() && app.stream === stream) receive(stream, message, threadId, generation);
      });
    }
    stream.onerror = () => {
      if (app.stream !== stream) {
        stream.close();
        return;
      }
      stream.close();
      app.stream = null;
      if (generation !== app.generation || threadId !== app.selectedThreadId) return;
      if (waitForOpen && !opened) {
        clearOpenHandshake();
        const reject = rejectOpen;
        resolveOpen = null;
        rejectOpen = null;
        reject?.(new Error("Runtime event stream did not reopen"));
        return;
      }
      reconnect();
    };
    return openHandshake;
  }

  return { connectStream, stopStream };
}

export function saveDraft(drafts, threadId, value) {
  if (!threadId) return;
  if (value) drafts.set(threadId, value);
  else drafts.delete(threadId);
}

export function restoreDraft(drafts, threadId) {
  return drafts.get(threadId) || "";
}

export function pendingAttentionCount(summary) {
  const count = Number(summary?.pending_attention_count);
  return Number.isSafeInteger(count) && count > 0 ? count : 0;
}

// Preserve the Runtime's newest-first order within each group. The server's
// typed pending-request count is the only attention authority; status prose
// and turn lifecycle strings deliberately do not participate.
export function groupThreadSummaries(summaries) {
  const groups = { needsYou: [], recent: [] };
  for (const summary of Array.isArray(summaries) ? summaries : []) {
    const group = pendingAttentionCount(summary) > 0 ? groups.needsYou : groups.recent;
    group.push(summary);
  }
  return groups;
}

export function pendingAttentionLabel(summary) {
  const count = pendingAttentionCount(summary);
  return count === 1
    ? "1 item needs your attention"
    : `${count} items need your attention`;
}

// Match the CWC composer grammar while keeping the embedded client free of a
// framework dependency: Enter sends, Shift+Enter inserts a newline, and an
// active IME composition is never interrupted.
export function isComposerSubmitKey({ key, shiftKey = false, isComposing = false } = {}) {
  return key === "Enter" && !shiftKey && !isComposing;
}

export function newThreadDefaults(catalog) {
  const providers = Array.isArray(catalog?.providers)
    ? catalog.providers.filter((provider) => String(provider?.id || "").trim())
    : [];
  const current = String(catalog?.current || "").trim();
  const provider = providers.find((entry) => entry.id === current) || providers[0] || null;
  return {
    providerId: String(provider?.id || "").trim(),
    modelProviderId: String(provider?.model_provider_id || "").trim(),
    model: String(provider?.default_model || "").trim(),
  };
}

export function imageInputPresentation(value) {
  if (value === "supported") {
    return {
      state: "supported",
      label: "Vision",
      description: "This exact provider route supports image input. Browser attachments are not enabled yet.",
    };
  }
  if (value === "unsupported") {
    return {
      state: "unsupported",
      label: "Text only",
      description: "This exact provider route does not support image input.",
    };
  }
  return {
    state: "unknown",
    label: "Image support unverified",
    description: "Image-input support is not verified for this exact provider route.",
  };
}

export function modelOptionLabel(model) {
  const id = String(model?.id || "").trim();
  return model?.image_input === "supported" ? `${id} · Vision` : id;
}

export function providerOptionLabel(provider) {
  const id = String(provider?.id || "").trim();
  const displayName = String(provider?.display_name || id).trim();
  const exactId = String(provider?.model_provider_id || "").trim();
  return exactId && exactId !== id ? `${displayName} · ${exactId}` : displayName;
}

export function buildCreateThreadRequest(providerId, model, modelProviderId = "") {
  const modelProvider = String(providerId || "").trim();
  const exactProviderId = String(modelProviderId || "").trim();
  const selectedModel = String(model || "").trim();
  if (!modelProvider || !selectedModel) {
    throw new Error("Choose both a provider and a model.");
  }
  const request = { model_provider: modelProvider, model: selectedModel };
  if (exactProviderId) request.model_provider_id = exactProviderId;
  return request;
}

export function threadProviderLabel(thread) {
  const exact = String(thread?.model_provider_id || "").trim();
  const generic = String(thread?.model_provider || "").trim();
  return exact || generic;
}

export function claimInFlight(inFlight, key) {
  const action = String(key || "").trim();
  if (!(inFlight instanceof Set) || !action || inFlight.has(action)) return false;
  inFlight.add(action);
  return true;
}

export function setSafeText(element, value) {
  element.textContent = value == null ? "" : String(value);
  return element;
}

function normalizedSequence(value) {
  const sequence = Number(value);
  return Number.isSafeInteger(sequence) && sequence > 0 ? sequence : 0;
}

function upsertTurn(state, turn) {
  if (!turn || !turn.id) return;
  if (!state.turns.has(turn.id)) state.turnOrder.push(turn.id);
  state.turns.set(turn.id, turn);
}

function upsertItem(state, item) {
  if (!item || !item.id) return;
  if (!state.items.has(item.id)) state.itemOrder.push(item.id);
  state.items.set(item.id, item);
}

function appendItemDelta(state, itemId, payload) {
  if (!itemId) return;
  const delta = typeof payload.delta === "string" ? payload.delta : "";
  const existing = state.items.get(itemId) || {
    id: itemId,
    turn_id: "",
    kind: payload.kind || "agent_message",
    status: "in_progress",
    summary: "",
    detail: "",
  };
  if (!state.items.has(itemId)) state.itemOrder.push(itemId);
  state.items.set(itemId, {
    ...existing,
    status: "in_progress",
    detail: `${existing.detail || ""}${delta}`,
  });
}

const PROVIDER_MODELS_PAGE_SIZE = 250;
const MAX_PROVIDER_MODELS = 10_000;
const MAX_PROVIDER_MODEL_PAGES = Math.ceil(
  MAX_PROVIDER_MODELS / PROVIDER_MODELS_PAGE_SIZE,
);

/**
 * Load every bounded page of one provider catalog.
 *
 * `fetchPage` is injected so the browser client can retain its authenticated
 * Runtime API boundary and tests can prove catalogs larger than one page are
 * not silently truncated. Cursors are opaque and may never repeat.
 */
export async function collectProviderModelPages(providerId, fetchPage) {
  const provider = String(providerId || "").trim();
  if (!provider || typeof fetchPage !== "function") {
    throw new Error("A provider and page loader are required.");
  }

  const entries = [];
  const seenCursors = new Set();
  let expectedTotal;
  let cursor = "";
  for (let page = 0; page < MAX_PROVIDER_MODEL_PAGES; page += 1) {
    const query = new URLSearchParams({ limit: String(PROVIDER_MODELS_PAGE_SIZE) });
    if (cursor) query.set("cursor", cursor);
    const response = await fetchPage(
      `/v1/providers/${encodeURIComponent(provider)}/models?${query.toString()}`,
    );
    if (String(response?.provider || "") !== provider) {
      throw new Error("The Runtime returned a model page for a different provider.");
    }
    if (!Array.isArray(response?.models)
      || response.models.length > PROVIDER_MODELS_PAGE_SIZE
      || !Number.isSafeInteger(response.total)
      || response.total < 0
      || response.total > MAX_PROVIDER_MODELS) {
      throw new Error("The Runtime returned an invalid provider model page.");
    }
    if (expectedTotal !== undefined && expectedTotal !== response.total) {
      throw new Error("The provider catalog changed; restart loading its models.");
    }
    expectedTotal = response.total;
    const pageEntries = response.models;
    if (entries.length + pageEntries.length > MAX_PROVIDER_MODELS) {
      throw new Error(`The provider catalog exceeds ${MAX_PROVIDER_MODELS} models.`);
    }
    entries.push(...pageEntries);

    const nextCursor = typeof response?.nextCursor === "string"
      ? response.nextCursor.trim()
      : "";
    if (!nextCursor) {
      if (entries.length !== expectedTotal) {
        throw new Error("The Runtime returned an incomplete provider model catalog.");
      }
      return entries;
    }
    if (pageEntries.length === 0 || seenCursors.has(nextCursor)) {
      throw new Error("The Runtime returned a non-progressing model cursor.");
    }
    if (entries.length >= expectedTotal) {
      throw new Error("The Runtime returned a cursor beyond its provider model catalog.");
    }
    seenCursors.add(nextCursor);
    cursor = nextCursor;
  }
  throw new Error(`The provider catalog exceeds ${MAX_PROVIDER_MODELS} models.`);
}

function startBrowserClient() {
  const dom = {
    shell: document.querySelector("#app-shell"),
    rail: document.querySelector("#thread-rail"),
    railOpen: document.querySelector("#rail-open"),
    railClose: document.querySelector("#rail-close"),
    railScrim: document.querySelector("#rail-scrim"),
    search: document.querySelector("#thread-search"),
    threadList: document.querySelector("#thread-list"),
    newThread: document.querySelector("#new-thread"),
    newThreadDialog: document.querySelector("#new-thread-dialog"),
    newThreadForm: document.querySelector("#new-thread-form"),
    newThreadProvider: document.querySelector("#new-thread-provider"),
    newThreadModel: document.querySelector("#new-thread-model"),
    newThreadModelSelectField: document.querySelector("#new-thread-model-select-field"),
    newThreadModelInput: document.querySelector("#new-thread-model-input"),
    newThreadModelInputField: document.querySelector("#new-thread-model-input-field"),
    newThreadCapability: document.querySelector("#new-thread-capability"),
    newThreadStatus: document.querySelector("#new-thread-status"),
    newThreadCancel: document.querySelector("#new-thread-cancel"),
    newThreadCreate: document.querySelector("#new-thread-create"),
    connectionDot: document.querySelector("#connection-dot"),
    connectionLabel: document.querySelector("#connection-label"),
    runtimeProvenance: document.querySelector("#runtime-provenance"),
    kicker: document.querySelector("#session-kicker"),
    title: document.querySelector("#session-title"),
    facts: document.querySelector("#session-facts"),
    rename: document.querySelector("#rename-thread"),
    archive: document.querySelector("#archive-thread"),
    status: document.querySelector("#status-banner"),
    transcript: document.querySelector("#transcript"),
    attention: document.querySelector("#attention"),
    composer: document.querySelector("#composer"),
    composerInput: document.querySelector("#composer-input"),
    send: document.querySelector("#send-message"),
    interrupt: document.querySelector("#interrupt-turn"),
    renameDialog: document.querySelector("#rename-dialog"),
    renameForm: document.querySelector("#rename-form"),
    renameInput: document.querySelector("#rename-input"),
    peek: document.querySelector("#session-peek"),
    savedSessions: document.querySelector("#saved-sessions"),
    sessionList: document.querySelector("#session-list"),
    session: document.querySelector(".session"),
  };

  const app = {
    summaries: [],
    sessionSummaries: [],
    // Typed selection: `none`, a read-only `session`, or a live `thread`.
    // Every reply/approval authority check reads this, not a loose id.
    target: NO_TARGET,
    // Bounded, redacted peek for the selected saved session, or null.
    peek: null,
    // Set when the SSE stream reported a sequence gap and a re-snapshot is
    // pending. Surfaced in the connection label rather than hidden.
    streamGap: false,
    selectedThreadId: "",
    threadState: createThreadState(),
    workspace: null,
    runtimeInfo: null,
    drafts: new Map(),
    stream: null,
    streamOpenCancel: null,
    reconnectTimer: null,
    generation: 0,
    searchTimer: null,
    railReturnFocus: null,
    inFlightActions: new Set(),
    providerCatalog: null,
    newThreadModels: [],
    newThreadGeneration: 0,
    newThreadLoading: false,
    creatingThread: false,
  };

  const narrowRail = globalThis.matchMedia("(max-width: 800px)");
  const composerSendAction = "composer-send";

  function element(tag, className, text) {
    const created = document.createElement(tag);
    if (className) created.className = className;
    if (text != null) setSafeText(created, text);
    return created;
  }

  function setInert(element, inert) {
    element.inert = inert;
    if (inert) element.setAttribute("inert", "");
    else element.removeAttribute("inert");
  }

  function applyDesktopRailAccessibility() {
    dom.shell.classList.remove("rail-visible");
    dom.rail.removeAttribute("aria-hidden");
    dom.rail.removeAttribute("aria-modal");
    dom.rail.removeAttribute("role");
    dom.session.removeAttribute("aria-hidden");
    setInert(dom.rail, false);
    setInert(dom.session, false);
    dom.railScrim.hidden = true;
    dom.railOpen.setAttribute("aria-expanded", "false");
    app.railReturnFocus = null;
  }

  function applyClosedMobileRailAccessibility() {
    dom.shell.classList.remove("rail-visible");
    dom.session.removeAttribute("aria-hidden");
    setInert(dom.session, false);
    dom.rail.setAttribute("role", "dialog");
    dom.rail.setAttribute("aria-modal", "true");
    dom.rail.setAttribute("aria-hidden", "true");
    setInert(dom.rail, true);
    dom.railScrim.hidden = true;
    dom.railOpen.setAttribute("aria-expanded", "false");
  }

  function openRail() {
    if (!narrowRail.matches) return;
    app.railReturnFocus = document.activeElement;
    dom.rail.setAttribute("role", "dialog");
    dom.rail.setAttribute("aria-modal", "true");
    dom.rail.setAttribute("aria-hidden", "false");
    setInert(dom.rail, false);
    dom.railScrim.hidden = false;
    dom.shell.classList.add("rail-visible");
    dom.railOpen.setAttribute("aria-expanded", "true");
    dom.railClose.focus({ preventScroll: true });
    dom.session.setAttribute("aria-hidden", "true");
    setInert(dom.session, true);
  }

  function closeRail({ restoreFocus = true } = {}) {
    if (!narrowRail.matches) {
      applyDesktopRailAccessibility();
      return;
    }
    dom.session.removeAttribute("aria-hidden");
    setInert(dom.session, false);
    const returnTarget = app.railReturnFocus?.isConnected
      ? app.railReturnFocus
      : dom.railOpen;
    if (restoreFocus) returnTarget.focus({ preventScroll: true });
    applyClosedMobileRailAccessibility();
    app.railReturnFocus = null;
  }

  function syncRailAccessibility() {
    if (!narrowRail.matches) {
      applyDesktopRailAccessibility();
      return;
    }
    if (dom.shell.classList.contains("rail-visible")) {
      dom.rail.setAttribute("role", "dialog");
      dom.rail.setAttribute("aria-modal", "true");
      dom.rail.setAttribute("aria-hidden", "false");
      setInert(dom.rail, false);
      dom.session.setAttribute("aria-hidden", "true");
      setInert(dom.session, true);
      dom.railScrim.hidden = false;
      dom.railOpen.setAttribute("aria-expanded", "true");
      return;
    }
    if (dom.rail.contains(document.activeElement)) {
      dom.railOpen.focus({ preventScroll: true });
    }
    applyClosedMobileRailAccessibility();
  }

  function syncVisualViewport() {
    const viewport = globalThis.visualViewport;
    const height = viewport?.height || globalThis.innerHeight;
    const offsetTop = viewport?.offsetTop || 0;
    if (Number.isFinite(height) && height > 0) {
      dom.shell.style.setProperty("--visual-viewport-height", `${Math.round(height)}px`);
    }
    dom.shell.style.setProperty("--visual-viewport-offset-top", `${Math.max(0, Math.round(offsetTop))}px`);
  }

  function focusableWithin(container) {
    return [...container.querySelectorAll(
      'button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
    )].filter((node) => !node.closest("[hidden]") && node.getAttribute("aria-hidden") !== "true");
  }

  function trapFocusWithin(event, container) {
    if (event.key !== "Tab") return false;
    const focusable = focusableWithin(container);
    if (focusable.length === 0) {
      event.preventDefault();
      container.focus({ preventScroll: true });
      return true;
    }
    const first = focusable[0];
    const last = focusable.at(-1);
    const active = document.activeElement;
    if (event.shiftKey && (active === first || !container.contains(active))) {
      event.preventDefault();
      last.focus({ preventScroll: true });
      return true;
    }
    if (!event.shiftKey && (active === last || !container.contains(active))) {
      event.preventDefault();
      first.focus({ preventScroll: true });
      return true;
    }
    return false;
  }

  function trapRailFocus(event) {
    if (
      event.key !== "Tab"
      || !narrowRail.matches
      || !dom.shell.classList.contains("rail-visible")
    ) return false;
    return trapFocusWithin(event, dom.rail);
  }

  function setConnection(kind, message) {
    dom.connectionDot.className = `connection-dot ${kind || ""}`.trim();
    setSafeText(dom.connectionLabel, message);
  }

  function showStatus(message) {
    setSafeText(dom.status, message || "");
    dom.status.hidden = !message;
  }

  const sessionFetch = createWebSessionFetch({
    location: window.location,
    history: window.history,
    storage: () => window.sessionStorage,
    pageProof: document.querySelector('meta[name="codewhale-web-request"]')?.content,
    fetch: (...args) => fetch(...args),
  });

  async function api(path, options = {}) {
    const response = await sessionFetch(path, options);
    if (!response.ok) {
      let message = `${response.status} ${response.statusText}`.trim();
      try {
        const body = await response.json();
        message = body?.error?.message || body?.message || message;
      } catch (_error) {
        // The status line is enough when the response is not JSON.
      }
      if (response.status === 401) {
        message = "This browser session is not authenticated. Restart `codewhale web` to open a fresh one-time session.";
      }
      throw Object.assign(new Error(message), { status: response.status });
    }
    if (response.status === 204) return null;
    const contentType = response.headers.get("content-type") || "";
    return contentType.includes("application/json") ? response.json() : response.text();
  }

  function renderThreadList() {
    dom.threadList.replaceChildren();
    if (app.summaries.length === 0) {
      const empty = element("p", "thread-preview", "No matching threads");
      empty.style.padding = "8px 10px";
      dom.threadList.append(empty);
      return;
    }

    const groups = groupThreadSummaries(app.summaries);
    if (groups.needsYou.length > 0) {
      appendThreadGroup("Needs you", "needs-you", groups.needsYou);
    }
    if (groups.recent.length > 0) {
      appendThreadGroup("Recent", "recent", groups.recent);
    }
  }

  function appendThreadGroup(label, idSuffix, summaries) {
    const group = element("section", `thread-group thread-group-${idSuffix}`);
    const headingId = `thread-group-${idSuffix}-title`;
    const heading = element("h2", "rail-section-title thread-group-title", label);
    heading.id = headingId;
    group.setAttribute("aria-labelledby", headingId);
    group.append(heading);

    for (const summary of summaries) {
      const row = element("button", "thread-row");
      row.type = "button";
      row.dataset.threadId = summary.id;
      row.setAttribute("aria-current", summary.id === app.selectedThreadId ? "true" : "false");
      const titleRow = element("span", "thread-title-row");
      titleRow.append(element("span", "thread-title", summary.title || "New thread"));
      const indicators = element("span", "thread-row-indicators");
      const attentionCount = pendingAttentionCount(summary);
      if (attentionCount > 0) {
        const attention = element("span", "thread-attention-count", String(attentionCount));
        attention.setAttribute("aria-label", pendingAttentionLabel(summary));
        indicators.append(attention);
      }
      const status = element("span", `status-pip ${summary.latest_turn_status === "inprogress" || summary.latest_turn_status === "in_progress" ? "running" : summary.latest_turn_status === "failed" ? "failed" : ""}`);
      status.setAttribute("aria-label", summary.latest_turn_status || "idle");
      indicators.append(status);
      titleRow.append(indicators);
      row.append(titleRow);
      row.append(element("span", "thread-preview", summary.preview || "No messages yet"));
      const branch = summary.branch || basename(summary.workspace) || "local";
      row.append(element("span", "thread-meta", `${branch} · ${relativeTime(summary.updated_at)}`));
      row.addEventListener("click", () => selectThread(summary.id));
      group.append(row);
    }
    dom.threadList.append(group);
  }

  async function loadThreads(search = dom.search.value.trim()) {
    const query = new URLSearchParams({ limit: "100" });
    if (search) query.set("search", search);
    app.summaries = await api(`/v1/threads/summary?${query.toString()}`);
    renderThreadList();
    return app.summaries;
  }

  // Saved sessions are the durable session store the terminal browses. They
  // are rendered with the same row shape as threads because
  // /v1/sessions/summary and /v1/threads/summary are field-compatible
  // projections — one vocabulary, not two.
  function renderSessionList() {
    dom.sessionList.replaceChildren();
    // The section only exists when the backend actually returned sessions;
    // an affordance for an empty store would imply a capability that has
    // nothing behind it.
    dom.savedSessions.hidden = app.sessionSummaries.length === 0;
    if (app.sessionSummaries.length === 0) return;

    for (const summary of app.sessionSummaries) {
      const row = element("button", "thread-row");
      row.type = "button";
      row.dataset.sessionId = summary.id;
      const titleRow = element("span", "thread-title-row");
      titleRow.append(element("span", "thread-title", summary.title || "Untitled session"));
      row.append(titleRow);
      row.append(element("span", "thread-preview", summary.preview || summary.title));
      const scope = basename(summary.workspace) || "local";
      row.append(
        element(
          "span",
          "thread-meta",
          `${scope} · ${summary.message_count} msg · ${relativeTime(summary.updated_at)}`,
        ),
      );
      row.setAttribute(
        "aria-current",
        app.target.kind === "session" && app.target.sessionId === summary.id ? "true" : "false",
      );
      // Click peeks; resuming is a separate, explicit button inside the peek.
      row.addEventListener("click", () => peekSession(summary.id));
      dom.sessionList.append(row);
    }
  }

  async function loadSessions(search = dom.search.value.trim()) {
    const query = new URLSearchParams({ limit: "50" });
    if (search) query.set("search", search);
    try {
      app.sessionSummaries = await api(`/v1/sessions/summary?${query.toString()}`);
    } catch (_error) {
      // A runtime without a readable session store is not a broken dashboard;
      // hide the section rather than blocking the thread view behind an error.
      app.sessionSummaries = [];
    }
    renderSessionList();
    return app.sessionSummaries;
  }

  // Resume goes through the existing endpoint, which seeds a real thread from
  // the saved messages. The dashboard does not reconstruct history itself.
  // Selecting a saved session shows a read-only peek. It does NOT resume:
  // resuming spawns a real thread and an engine, which must be a deliberate
  // act, not a side effect of clicking a row to see what it was about.
  async function peekSession(sessionId) {
    stopStream();
    app.selectedThreadId = "";
    app.threadState = createThreadState();
    app.target = sessionTarget(sessionId);
    showStatus("");
    renderThreadList();
    renderSessionList();
    try {
      // `?peek=true` returns a bounded, redacted projection — twelve entries,
      // tool payloads summarised — so the browser never receives the full
      // transcript in order to display a preview of it.
      app.peek = await api(
        `/v1/sessions/${encodeURIComponent(sessionId)}?peek=true&entries=12`,
      );
    } catch (error) {
      app.peek = null;
      showStatus(error.message);
    }
    renderAll();
  }

  async function resumeSession(sessionId) {
    showStatus("");
    try {
      const resumed = await api(`/v1/sessions/${encodeURIComponent(sessionId)}/resume-thread`, {
        method: "POST",
        body: "{}",
      });
      app.peek = null;
      await loadThreads("");
      // `selectThread` sets the live thread target; only after this can the
      // composer or an approval act.
      await selectThread(resumed.thread_id);
      showStatus(resumed.summary || "");
    } catch (error) {
      showStatus(error.message);
    }
  }

  // Render the read-only peek pane for a selected saved session.
  function renderPeek() {
    if (!dom.peek) return;
    const showing = app.target.kind === "session" && app.peek;
    dom.peek.hidden = !showing;
    if (!showing) {
      dom.peek.replaceChildren();
      return;
    }
    const peek = app.peek;
    dom.peek.replaceChildren();

    const header = element("div", "peek-header");
    header.append(element("p", "eyebrow", "Saved session — read only"));
    header.append(element("h2", "", peek.title || "Untitled session"));
    header.append(
      element(
        "p",
        "thread-meta",
        `${basename(peek.workspace) || "local"} · ${peek.message_count} messages · ${relativeTime(peek.updated_at)}${peek.archived ? " · archived" : ""}`,
      ),
    );
    dom.peek.append(header);

    if (peek.omitted_before > 0) {
      dom.peek.append(
        element("p", "peek-omitted", `${peek.omitted_before} earlier messages not shown`),
      );
    }

    for (const entry of peek.entries || []) {
      const row = element("div", `peek-entry peek-${entry.kind}`);
      row.append(element("span", "peek-kind", entry.kind));
      // `element()` assigns via textContent. Peek text is recorded user/model
      // content and must never reach an HTML sink; this is the XSS boundary.
      row.append(element("p", "peek-text", entry.text));
      if (entry.redacted) row.append(element("span", "peek-flag", "redacted"));
      if (entry.truncated) row.append(element("span", "peek-flag", "truncated"));
      dom.peek.append(row);
    }

    const resume = element("button", "primary-button", "Resume into a live thread");
    resume.type = "button";
    resume.addEventListener("click", () => resumeSession(peek.session_id));
    dom.peek.append(resume);
  }

  const { connectStream, stopStream } = createStreamConnector({
    app, api, EventSource, setConnection, showStatus,
    receive: (stream, message, threadId, generation) => {
      try {
        const envelope = JSON.parse(message.data);
        if (runtimeEventContinuity(app.threadState, envelope) === "gap") {
          app.streamGap = true;
          renderStreamCursor();
          showStatus("Runtime event continuity changed; refreshing the thread snapshot…");
          void recoverProjection(threadId, generation, stream);
          return;
        }
        if (!applyRuntimeEvent(app.threadState, envelope)) return;
        renderAll(true);
        if (
          envelope.event === "turn.completed"
          || envelope.event === "thread.updated"
          || envelope.event === "approval.required"
          || envelope.event === "approval.decided"
          || envelope.event === "approval.timeout"
          || envelope.event === "user_input.required"
          || envelope.event === "user_input.answered"
          || envelope.event === "user_input.canceled"
        ) {
          loadThreads().catch((error) => showStatus(error.message));
        }
      } catch (error) {
        showStatus(`Could not read a Runtime event: ${error.message}`);
      }
    },
  });

  async function selectThread(threadId) {
    if (!threadId) return;
    saveDraft(app.drafts, app.selectedThreadId, dom.composerInput.value);
    stopStream();
    app.selectedThreadId = threadId;
    // A live thread is now the target: from here the composer and approvals
    // may act. Clear any saved-session peek so the two surfaces are exclusive.
    app.target = threadTarget(threadId);
    app.peek = null;
    app.streamGap = false;
    app.threadState = createThreadState(threadId);
    app.generation += 1;
    const generation = app.generation;
    dom.composerInput.value = restoreDraft(app.drafts, threadId);
    resizeComposer();
    renderThreadList();
    renderSessionList();
    renderAll();
    closeRailIfNarrow();
    setConnection("", "Loading thread snapshot…");
    showStatus("");

    try {
      const subscribed = await snapshotThenSubscribe({
        state: app.threadState,
        threadId,
        loadSnapshot: (id) => api(`/v1/threads/${encodeURIComponent(id)}`),
        subscribe: (id, sequence) => {
          return connectStream(id, sequence, generation);
        },
        isCurrent: () => generation === app.generation && threadId === app.selectedThreadId,
      });
      if (!subscribed) return;
      renderAll();
      setConnection("ready", "Local runtime connected");
    } catch (error) {
      if (generation !== app.generation) return;
      showStatus(error.message);
      setConnection("error", "Runtime connection failed");
    }
  }

  async function recoverProjection(threadId, generation, sourceStream = null) {
    if (
      generation !== app.generation
      || threadId !== app.selectedThreadId
      || (sourceStream && app.stream !== sourceStream)
    ) return;

    stopStream();
    setConnection("", "Refreshing thread snapshot…");

    try {
      const subscribed = await recoverSnapshotAndSubscribe({
        state: app.threadState,
        threadId,
        loadSnapshot: (id) => api(`/v1/threads/${encodeURIComponent(id)}`),
        subscribe: (id, sequence) => connectStream(id, sequence, generation, true),
        isCurrent: () => generation === app.generation && threadId === app.selectedThreadId,
      }, () => {
        // A gap is continuous again only after both the replacement snapshot
        // and the replacement EventSource open handshake have succeeded.
        app.streamGap = false;
      });
      if (!subscribed) return;
      renderAll();
      showStatus("");
      setConnection("ready", "Local runtime connected");
    } catch (error) {
      if (generation !== app.generation || threadId !== app.selectedThreadId) return;
      showStatus(`Could not refresh the thread snapshot: ${error.message}`);
      setConnection("error", "Runtime recovery failed");
      if (error.status !== 401 && error.status !== 403) {
        app.reconnectTimer = setTimeout(
          () => recoverProjection(threadId, generation),
          900,
        );
      }
    }
  }

  function renderAll(preserveScroll = false) {
    renderHeader();
    renderPeek();
    renderTranscript(preserveScroll);
    renderAttention();
    renderComposer();
    renderStreamCursor();
  }

  // Show the SSE resume cursor so "am I reading everything?" is answerable.
  function renderStreamCursor() {
    if (app.target.kind !== "thread") return;
    const cursor = streamCursor(app.threadState, {
      gap: app.streamGap,
      connected: Boolean(app.stream),
    });
    setConnection(cursor.gap ? "error" : cursor.connected ? "ready" : "", cursor.label);
  }

  function renderHeader() {
    const thread = app.threadState.thread;
    const summary = app.summaries.find((item) => item.id === app.selectedThreadId);
    const title = thread?.title || summary?.title || (thread ? "New thread" : "Choose a thread");
    setSafeText(dom.title, title);
    setSafeText(dom.kicker, thread ? "Local Runtime thread" : "Local Runtime");
    dom.rename.disabled = !thread;
    dom.archive.disabled = !thread;
    dom.facts.replaceChildren();
    if (!thread) return;

    const workspace = summary?.workspace || thread.workspace || app.workspace?.workspace;
    const branch = summary?.branch || app.workspace?.branch;
    dom.facts.append(factChip("Workspace", basename(workspace) || "local"));
    if (branch) dom.facts.append(factChip("Branch", branch));
    const provider = threadProviderLabel(thread);
    if (provider) dom.facts.append(factChip("Provider", provider));
    dom.facts.append(factChip("Model", thread.model || "Runtime default"));
    dom.facts.append(factChip("Mode", modeLabel(thread.mode)));
    dom.facts.append(factChip("Permission", permissionLabel(thread)));
  }

  function factChip(label, value) {
    const chip = element("span", "fact-chip");
    chip.dataset.fact = String(label || "").toLowerCase();
    chip.append(element("span", "", label));
    chip.append(element("strong", "", value));
    return chip;
  }

  function renderTranscript(preserveScroll) {
    const wasNearBottom = dom.transcript.scrollHeight - dom.transcript.scrollTop - dom.transcript.clientHeight < 120;
    if (!app.threadState.thread) {
      renderTranscriptEmpty(
        "choose-thread",
        "Your local agent, in the browser.",
        "Create a thread or choose one from the rail. This client uses the same Runtime as the terminal.",
      );
      return;
    }
    if (app.threadState.itemOrder.length === 0) {
      renderTranscriptEmpty(
        "ready",
        "Ready for a task.",
        "Send a message below. Model, mode, and permission posture come from the Runtime and are shown read-only above.",
      );
      return;
    }

    const selection = captureTranscriptSelection();
    const existing = new Map(
      [...dom.transcript.children]
        .filter((node) => node.dataset.itemId)
        .map((node) => [node.dataset.itemId, node]),
    );
    const desired = [];
    for (const itemId of app.threadState.itemOrder) {
      const item = app.threadState.items.get(itemId);
      if (!item) continue;
      let node = existing.get(itemId);
      if (!node || !updateItemNode(node, item)) node = renderItem(item);
      desired.push(node);
    }
    reconcileChildren(dom.transcript, desired);
    restoreTranscriptSelection(selection);
    if (!preserveScroll || wasNearBottom) {
      requestAnimationFrame(() => {
        dom.transcript.scrollTop = dom.transcript.scrollHeight;
      });
    }
  }

  function renderTranscriptEmpty(kind, title, description) {
    const current = dom.transcript.children.length === 1
      ? dom.transcript.firstElementChild
      : null;
    if (current?.dataset.emptyState === kind) return;
    const empty = emptyState(title, description);
    empty.dataset.emptyState = kind;
    reconcileChildren(dom.transcript, [empty]);
  }

  function reconcileChildren(container, desired) {
    const keep = new Set(desired);
    let cursor = container.firstElementChild;
    for (const node of desired) {
      if (node === cursor) {
        cursor = cursor.nextElementSibling;
      } else {
        container.insertBefore(node, cursor);
      }
    }
    for (const child of [...container.children]) {
      if (!keep.has(child)) child.remove();
    }
  }

  function emptyState(title, description) {
    const empty = element("div", "empty-state");
    const mark = document.createElement("img");
    mark.className = "empty-mark";
    mark.src = "/assets/codewhale-192.png";
    mark.alt = "";
    empty.append(mark);
    empty.append(element("h2", "", title));
    empty.append(element("p", "", description));
    return empty;
  }

  function renderItem(item) {
    let card;
    if (item.kind === "user_message" || item.kind === "agent_message") {
      const role = item.kind === "user_message" ? "user" : "agent";
      card = element("article", `message ${role}`);
      const label = element("div", "message-label");
      label.dataset.itemPart = "label";
      const body = element("div", "message-body");
      body.dataset.itemPart = "body";
      card.append(label, body);
    } else if (item.kind === "agent_reasoning") {
      card = element("article", "reasoning");
      const disclosure = element("details");
      const summary = element("summary");
      summary.dataset.itemPart = "summary";
      const detail = element("pre");
      detail.dataset.itemPart = "detail";
      disclosure.append(summary, detail);
      card.append(disclosure);
    } else {
      card = element("article", "receipt");
      card.append(element("span", "receipt-dot"));
      const copy = element("span", "receipt-copy");
      const label = element("strong");
      label.dataset.itemPart = "label";
      const summary = element("span", "receipt-summary");
      summary.dataset.itemPart = "summary";
      copy.append(label, summary);
      card.append(copy);
    }
    card.dataset.itemId = item.id;
    card.dataset.itemKind = item.kind;
    updateItemNode(card, item);
    return card;
  }

  function updateItemNode(card, item) {
    if (card.dataset.itemKind !== item.kind) return false;
    const detail = item.detail || item.summary || "";
    if (item.kind === "user_message" || item.kind === "agent_message") {
      const role = item.kind === "user_message" ? "user" : "agent";
      card.className = `message ${role} ${item.status === "in_progress" ? "in-progress" : ""}`.trim();
      setTextIfChanged(card.querySelector('[data-item-part="label"]'), role === "user" ? "You" : "Codewhale");
      setTextIfChanged(card.querySelector('[data-item-part="body"]'), detail);
      return true;
    }
    if (item.kind === "agent_reasoning") {
      setTextIfChanged(
        card.querySelector('[data-item-part="summary"]'),
        item.status === "in_progress" ? "Reasoning…" : "Reasoning",
      );
      setTextIfChanged(card.querySelector('[data-item-part="detail"]'), detail);
      return true;
    }

    const presentation = receiptPresentation(item);
    card.className = `receipt ${presentation.failed ? "failed" : ""}`.trim();
    setTextIfChanged(card.querySelector('[data-item-part="label"]'), presentation.label);
    setTextIfChanged(card.querySelector('[data-item-part="summary"]'), presentation.summary);
    const copy = card.querySelector(".receipt-copy");
    let disclosure = copy.querySelector("details");
    if (presentation.raw && presentation.raw !== presentation.summary) {
      if (!disclosure) {
        disclosure = element("details");
        const summary = element("summary", "", "Show receipt");
        const raw = element("pre");
        raw.dataset.itemPart = "raw";
        disclosure.append(summary, raw);
        copy.append(disclosure);
      }
      setTextIfChanged(disclosure.querySelector('[data-item-part="raw"]'), presentation.raw);
    } else if (disclosure) {
      disclosure.remove();
    }
    return true;
  }

  function setTextIfChanged(target, value) {
    const next = value == null ? "" : String(value);
    if (target.textContent !== next) setSafeText(target, next);
  }

  function captureTranscriptSelection() {
    const selection = globalThis.getSelection?.();
    if (!selection || selection.rangeCount === 0 || selection.isCollapsed) return null;
    const range = selection.getRangeAt(0);
    const start = transcriptSelectionEndpoint(range.startContainer, range.startOffset);
    const end = transcriptSelectionEndpoint(range.endContainer, range.endOffset);
    return start && end ? { start, end } : null;
  }

  function transcriptSelectionEndpoint(node, offset) {
    const elementNode = node.nodeType === 1 ? node : node.parentElement;
    const item = elementNode?.closest?.("[data-item-id]");
    if (!item || !dom.transcript.contains(item)) return null;
    const prefix = document.createRange();
    prefix.selectNodeContents(item);
    try {
      prefix.setEnd(node, offset);
    } catch (_error) {
      return null;
    }
    return { itemId: item.dataset.itemId, offset: prefix.toString().length };
  }

  function restoreTranscriptSelection(captured) {
    if (!captured) return;
    const startRoot = [...dom.transcript.children]
      .find((node) => node.dataset.itemId === captured.start.itemId);
    const endRoot = [...dom.transcript.children]
      .find((node) => node.dataset.itemId === captured.end.itemId);
    if (!startRoot || !endRoot) return;
    const start = textPointAt(startRoot, captured.start.offset);
    const end = textPointAt(endRoot, captured.end.offset);
    if (!start || !end) return;
    const range = document.createRange();
    try {
      range.setStart(start.node, start.offset);
      range.setEnd(end.node, end.offset);
    } catch (_error) {
      return;
    }
    const selection = globalThis.getSelection?.();
    if (!selection) return;
    selection.removeAllRanges();
    selection.addRange(range);
  }

  function textPointAt(root, requestedOffset) {
    const walker = document.createTreeWalker(
      root,
      globalThis.NodeFilter?.SHOW_TEXT || 4,
    );
    let remaining = Math.max(0, requestedOffset);
    let last = null;
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      last = node;
      const length = node.data.length;
      if (remaining <= length) return { node, offset: remaining };
      remaining -= length;
    }
    return last ? { node: last, offset: last.data.length } : null;
  }

  function renderAttention() {
    const existing = new Map(
      [...dom.attention.children]
        .filter((node) => node.dataset.attentionKey)
        .map((node) => [node.dataset.attentionKey, node]),
    );
    const desired = [];
    for (const [approvalId, approval] of app.threadState.approvals) {
      const key = `approval:${approvalId}`;
      const card = existing.get(key) || renderApproval(approvalId, approval);
      card.dataset.attentionKey = key;
      setAttentionCardBusyNode(card, app.inFlightActions.has(key));
      desired.push(card);
    }
    for (const [inputId, input] of app.threadState.userInputs) {
      const key = `input:${inputId}`;
      const card = existing.get(key) || renderUserInput(inputId, input);
      card.dataset.attentionKey = key;
      setAttentionCardBusyNode(card, app.inFlightActions.has(key));
      desired.push(card);
    }
    reconcileChildren(dom.attention, desired);
    dom.attention.hidden = desired.length === 0;
  }

  function setAttentionCardBusy(key, busy) {
    const card = [...dom.attention.children]
      .find((node) => node.dataset.attentionKey === key);
    if (card) setAttentionCardBusyNode(card, busy);
  }

  function setAttentionCardBusyNode(card, busy) {
    card.setAttribute("aria-busy", busy ? "true" : "false");
    for (const control of card.querySelectorAll("button, input, textarea, select")) {
      control.disabled = busy;
    }
  }

  function renderApproval(approvalId, approval) {
    const card = element("article", "attention-card");
    const titleId = `attention-approval-${safeDomId(approvalId)}`;
    card.setAttribute("role", "group");
    card.setAttribute("aria-labelledby", titleId);
    card.append(element("p", "eyebrow", "Approval required"));
    const title = element("h2", "", approval.tool_name || "Tool request");
    title.id = titleId;
    card.append(title);
    card.append(element("p", "", approval.intent_summary || approval.description || "Codewhale is waiting for permission."));
    const actions = element("div", "attention-actions");
    const rememberLabel = element("label", "remember-field");
    const remember = document.createElement("input");
    remember.type = "checkbox";
    rememberLabel.append(remember, document.createTextNode("Remember for this thread"));
    const deny = element("button", "quiet-button danger", "Deny");
    deny.type = "button";
    deny.addEventListener("click", () => resolveApproval(approvalId, "deny", remember.checked));
    const allow = element("button", "primary-button", "Allow");
    allow.type = "button";
    allow.addEventListener("click", () => resolveApproval(approvalId, "allow", remember.checked));
    actions.append(rememberLabel, deny, allow);
    card.append(actions);
    return card;
  }

  async function resolveApproval(approvalId, decision, remember) {
    // Authority check before authority action. The approval must belong to the
    // thread we are actually watching, and that thread must be the selected
    // live target — never a saved-session peek, never a row the user has since
    // moved off. Refusals are loud and send nothing.
    const resolved = resolveApprovalTarget(approvalId, app.target, app.threadState);
    if (!resolved.ok) {
      showStatus(refusalMessage(resolved.reason));
      renderAttention();
      return;
    }
    const action = `approval:${approvalId}`;
    if (!claimInFlight(app.inFlightActions, action)) return;
    setAttentionCardBusy(action, true);
    try {
      await api(`/v1/approvals/${encodeURIComponent(resolved.approvalId)}`, {
        method: "POST",
        body: JSON.stringify({ decision, remember }),
      });
      app.threadState.approvals.delete(approvalId);
      showStatus("");
      renderAttention();
    } catch (error) {
      showStatus(error.message);
    } finally {
      app.inFlightActions.delete(action);
      setAttentionCardBusy(action, false);
    }
  }

  function renderUserInput(inputId, envelope) {
    const card = element("form", "attention-card");
    const titleId = `attention-input-${safeDomId(inputId)}`;
    card.setAttribute("role", "group");
    card.setAttribute("aria-labelledby", titleId);
    card.append(element("p", "eyebrow", "Input required"));
    const title = element("h2", "", "Codewhale has a question");
    title.id = titleId;
    card.append(title);
    const questions = Array.isArray(envelope.request?.questions) ? envelope.request.questions : [];
    const groups = [];
    for (const question of questions) {
      const fieldset = element("fieldset", "question-fieldset");
      fieldset.append(element("legend", "", question.question || question.header || "Choose an option"));
      const controls = [];
      for (const option of Array.isArray(question.options) ? question.options : []) {
        const label = element("label", "answer-option");
        const input = document.createElement("input");
        input.type = question.multi_select ? "checkbox" : "radio";
        input.name = `question-${inputId}-${question.id}`;
        input.value = option.label || "";
        label.append(input);
        const copy = element("span", "", option.label || "Option");
        if (option.description) copy.append(element("small", "", option.description));
        label.append(copy);
        fieldset.append(label);
        controls.push({ input, label: option.label || "", value: option.label || "" });
      }
      // Match the terminal surface: a custom answer stays available even when
      // an older request omitted or disabled the legacy allow_free_text hint.
      const other = document.createElement("input");
      other.className = "other-answer";
      other.type = "text";
      other.placeholder = "Other response";
      other.setAttribute("aria-label", `${question.header || "Question"} other response`);
      if (!question.multi_select) {
        other.addEventListener("input", () => {
          if (other.value.trim()) {
            for (const control of controls) control.input.checked = false;
          }
        });
        for (const control of controls) {
          control.input.addEventListener("change", () => {
            if (control.input.checked) other.value = "";
          });
        }
      }
      fieldset.append(other);
      card.append(fieldset);
      groups.push({ question, controls, other });
    }
    const actions = element("div", "attention-actions");
    const submit = element("button", "primary-button", "Submit answers");
    submit.type = "submit";
    actions.append(submit);
    card.append(actions);
    card.addEventListener("submit", async (event) => {
      event.preventDefault();
      const selections = new Map();
      const freeText = new Map();
      for (const group of groups) {
        const selected = [];
        for (const control of group.controls) {
          if (control.input.checked) selected.push(control.value);
        }
        selections.set(group.question.id, selected);
        freeText.set(group.question.id, group.other.value);
      }
      const resolved = resolveUserInputTarget(inputId, app.target, app.threadState);
      if (!resolved.ok) {
        showStatus(refusalMessage(resolved.reason));
        renderAttention();
        return;
      }
      const built = answersForUserInput(envelope.request, selections, freeText);
      if (!built.ok) {
        const message = built.reason === "missing-answer"
          ? `Choose an answer for ${built.question}.`
          : built.reason === "multiple-answers"
            ? `Choose one answer for ${built.question}.`
            : `That question changed before it could be submitted — nothing was sent.`;
        showStatus(message);
        return;
      }
      const action = `input:${inputId}`;
      if (!claimInFlight(app.inFlightActions, action)) return;
      setAttentionCardBusy(action, true);
      try {
        await api(`/v1/user-input/${encodeURIComponent(resolved.threadId)}/${encodeURIComponent(resolved.inputId)}`, {
          method: "POST",
          body: JSON.stringify({ answers: built.answers }),
        });
        app.threadState.userInputs.delete(inputId);
        showStatus("");
        renderAttention();
      } catch (error) {
        showStatus(error.message);
      } finally {
        app.inFlightActions.delete(action);
        setAttentionCardBusy(action, false);
      }
    });
    return card;
  }

  function safeDomId(value) {
    return String(value || "item").replace(/[^a-zA-Z0-9_-]/g, "-");
  }

  function latestTurn() {
    const id = app.threadState.turnOrder.at(-1);
    return id ? app.threadState.turns.get(id) : null;
  }

  function activeTurn() {
    const turn = latestTurn();
    return turn && (turn.status === "in_progress" || turn.status === "queued") ? turn : null;
  }

  function renderComposer() {
    const ready = Boolean(app.threadState.thread);
    const active = activeTurn();
    const sending = app.inFlightActions.has(composerSendAction);
    dom.composerInput.disabled = sending || !ready;
    dom.send.disabled = sending || !ready || !dom.composerInput.value.trim();
    dom.composer.setAttribute("aria-busy", sending ? "true" : "false");
    dom.interrupt.hidden = !active;
    setSafeText(dom.send, sending ? (active ? "Steering…" : "Sending…") : active ? "Steer" : "Send");
  }

  function selectedNewThreadProvider() {
    const providerId = dom.newThreadProvider.value;
    return app.providerCatalog?.providers?.find((provider) => provider.id === providerId) || null;
  }

  function selectedNewThreadModel() {
    const provider = selectedNewThreadProvider();
    return provider?.has_model_catalog
      ? dom.newThreadModel.value
      : dom.newThreadModelInput.value.trim();
  }

  function setNewThreadStatus(message, state = "") {
    setSafeText(dom.newThreadStatus, message || "");
    dom.newThreadStatus.dataset.state = state;
  }

  function renderNewThreadCapability() {
    const provider = selectedNewThreadProvider();
    const modelId = selectedNewThreadModel();
    if (!provider || !modelId) {
      setSafeText(dom.newThreadCapability, "");
      dom.newThreadCapability.dataset.state = "unknown";
      return;
    }
    const model = provider.has_model_catalog
      ? app.newThreadModels.find((entry) => entry.id === modelId)
      : null;
    const presentation = imageInputPresentation(model?.image_input);
    dom.newThreadCapability.dataset.state = presentation.state;
    setSafeText(
      dom.newThreadCapability,
      `${presentation.label} — ${presentation.description}`,
    );
  }

  function syncNewThreadControls() {
    const provider = selectedNewThreadProvider();
    const hasCatalog = Boolean(provider?.has_model_catalog);
    const busy = app.newThreadLoading || app.creatingThread;
    dom.newThreadProvider.disabled = app.creatingThread || !app.providerCatalog?.providers?.length;
    dom.newThreadModel.disabled = busy || !hasCatalog || app.newThreadModels.length === 0;
    dom.newThreadModelInput.disabled = busy || !provider || hasCatalog;
    dom.newThreadCancel.disabled = app.creatingThread;
    dom.newThreadCreate.disabled = busy || !provider || !selectedNewThreadModel();
    renderNewThreadCapability();
  }

  function setNewThreadModelSurface(provider) {
    const hasCatalog = Boolean(provider?.has_model_catalog);
    dom.newThreadModelSelectField.hidden = !hasCatalog;
    dom.newThreadModelInputField.hidden = hasCatalog;
  }

  async function loadNewThreadModels(providerId, preferredModel, generation) {
    if (generation !== app.newThreadGeneration || !dom.newThreadDialog.open) return;
    const provider = app.providerCatalog?.providers?.find((entry) => entry.id === providerId);
    app.newThreadModels = [];
    dom.newThreadModel.replaceChildren();
    dom.newThreadModelInput.value = "";
    setNewThreadModelSurface(provider);
    if (!provider) {
      app.newThreadLoading = false;
      setNewThreadStatus("Choose a provider.", "error");
      syncNewThreadControls();
      return;
    }

    const modelDefault = String(preferredModel || provider.default_model || "").trim();
    if (!provider.has_model_catalog) {
      dom.newThreadModelInput.value = modelDefault;
      app.newThreadLoading = false;
      setNewThreadStatus("");
      syncNewThreadControls();
      return;
    }

    app.newThreadLoading = true;
    setNewThreadStatus("Loading models…");
    syncNewThreadControls();
    try {
      const modelEntries = await collectProviderModelPages(provider.id, async (path) => {
        const page = await api(path);
        if (generation !== app.newThreadGeneration || !dom.newThreadDialog.open) {
          throw new Error("The model request was superseded.");
        }
        return page;
      });
      if (generation !== app.newThreadGeneration || !dom.newThreadDialog.open) return;
      const seen = new Set();
      const models = [];
      for (const entry of modelEntries) {
        const id = String(entry?.id || "").trim();
        const key = id.toLowerCase();
        if (!id || seen.has(key)) continue;
        seen.add(key);
        models.push({
          id,
          image_input: ["supported", "unsupported", "unknown"].includes(entry?.image_input)
            ? entry.image_input
            : "unknown",
        });
      }
      if (modelDefault && !seen.has(modelDefault.toLowerCase())) {
        models.unshift({ id: modelDefault, image_input: "unknown" });
      }
      app.newThreadModels = models;
      for (const model of models) {
        const option = document.createElement("option");
        option.value = model.id;
        setSafeText(option, modelOptionLabel(model));
        dom.newThreadModel.append(option);
      }
      const selectedDefault = models.find(
        (model) => model.id.toLowerCase() === modelDefault.toLowerCase(),
      );
      if (selectedDefault) dom.newThreadModel.value = selectedDefault.id;
      app.newThreadLoading = false;
      setNewThreadStatus(
        models.length ? "" : "No models are available for this provider.",
        models.length ? "" : "error",
      );
      syncNewThreadControls();
    } catch (error) {
      if (generation !== app.newThreadGeneration || !dom.newThreadDialog.open) return;
      app.newThreadLoading = false;
      setNewThreadStatus(`Could not load models: ${error.message}`, "error");
      syncNewThreadControls();
    }
  }

  async function openNewThreadDialog() {
    if (dom.newThreadDialog.open || app.creatingThread) return;
    dom.newThreadDialog.showModal();
    dom.newThreadCancel.focus({ preventScroll: true });
    const generation = ++app.newThreadGeneration;
    app.providerCatalog = null;
    app.newThreadModels = [];
    app.newThreadLoading = true;
    dom.newThreadProvider.replaceChildren();
    dom.newThreadModel.replaceChildren();
    dom.newThreadModelInput.value = "";
    setNewThreadStatus("Loading providers…");
    renderNewThreadCapability();
    syncNewThreadControls();
    try {
      const catalog = await api("/v1/providers");
      if (generation !== app.newThreadGeneration || !dom.newThreadDialog.open) return;
      const providers = Array.isArray(catalog?.providers)
        ? catalog.providers.filter((provider) => String(provider?.id || "").trim())
        : [];
      if (providers.length === 0) throw new Error("The Runtime returned no providers.");
      app.providerCatalog = { ...catalog, providers };
      for (const provider of providers) {
        const option = document.createElement("option");
        option.value = provider.id;
        setSafeText(option, providerOptionLabel(provider));
        dom.newThreadProvider.append(option);
      }
      const defaults = newThreadDefaults(app.providerCatalog);
      dom.newThreadProvider.value = defaults.providerId;
      dom.newThreadProvider.disabled = false;
      dom.newThreadProvider.focus({ preventScroll: true });
      await loadNewThreadModels(defaults.providerId, defaults.model, generation);
    } catch (error) {
      if (generation !== app.newThreadGeneration || !dom.newThreadDialog.open) return;
      app.newThreadLoading = false;
      setNewThreadStatus(`Could not load providers: ${error.message}`, "error");
      syncNewThreadControls();
    }
  }

  async function submitNewThread(event) {
    event.preventDefault();
    const provider = selectedNewThreadProvider();
    let request;
    try {
      request = buildCreateThreadRequest(
        dom.newThreadProvider.value,
        selectedNewThreadModel(),
        provider?.model_provider_id,
      );
    } catch (error) {
      setNewThreadStatus(error.message, "error");
      return;
    }
    app.creatingThread = true;
    setNewThreadStatus("Creating thread…");
    dom.newThreadDialog.focus({ preventScroll: true });
    syncNewThreadControls();
    const thread = await createThread(
      request,
      (message) => setNewThreadStatus(message, message ? "error" : ""),
    );
    app.creatingThread = false;
    if (thread) {
      dom.newThreadDialog.close();
      dom.composerInput.focus();
      return;
    }
    syncNewThreadControls();
    dom.newThreadProvider.focus({ preventScroll: true });
  }

  async function createThread(request = {}, reportError = showStatus) {
    showStatus("");
    reportError("");
    try {
      const thread = await api("/v1/threads", {
        method: "POST",
        body: JSON.stringify(request),
      });
      await loadThreads("");
      await selectThread(thread.id);
      dom.composerInput.focus();
      return thread;
    } catch (error) {
      reportError(error.message);
      return null;
    }
  }

  async function sendMessage() {
    const prompt = dom.composerInput.value.trim();
    if (!prompt) return;
    // A reply goes to a live thread or nowhere. A saved-session peek must not
    // silently resume-and-send: that would attach the user's message to a
    // thread they never asked to create.
    if (app.target.kind === "session") {
      showStatus(refusalMessage("session-not-live"));
      return;
    }
    if (!claimInFlight(app.inFlightActions, composerSendAction)) return;
    renderComposer();
    showStatus("");
    try {
      let threadId = app.selectedThreadId;
      if (!threadId) {
        const thread = await createThread();
        if (!thread) return;
        threadId = thread.id;
      }
      const resolved = resolveReplyTarget(threadTarget(threadId), app.threadState);
      if (!resolved.ok) {
        showStatus(refusalMessage(resolved.reason));
        return;
      }
      threadId = resolved.threadId;
      const turn = activeTurn();
      if (turn) {
        await api(`/v1/threads/${encodeURIComponent(threadId)}/turns/${encodeURIComponent(turn.id)}/steer`, {
          method: "POST",
          body: JSON.stringify({ prompt }),
        });
      } else {
        await api(`/v1/threads/${encodeURIComponent(threadId)}/turns`, {
          method: "POST",
          body: JSON.stringify({ prompt }),
        });
      }
      saveDraft(app.drafts, threadId, "");
      dom.composerInput.value = "";
      resizeComposer();
      renderComposer();
      loadThreads().catch((error) => showStatus(error.message));
    } catch (error) {
      showStatus(error.message);
    } finally {
      app.inFlightActions.delete(composerSendAction);
      renderComposer();
    }
  }

  async function interruptTurn() {
    const turn = activeTurn();
    if (!turn || !app.selectedThreadId) return;
    dom.interrupt.disabled = true;
    try {
      await api(`/v1/threads/${encodeURIComponent(app.selectedThreadId)}/turns/${encodeURIComponent(turn.id)}/interrupt`, { method: "POST" });
    } catch (error) {
      showStatus(error.message);
    } finally {
      dom.interrupt.disabled = false;
    }
  }

  async function archiveThread() {
    if (!app.selectedThreadId) return;
    if (!globalThis.confirm("Archive this thread? You can still access it through the Runtime API.")) return;
    try {
      await api(`/v1/threads/${encodeURIComponent(app.selectedThreadId)}`, {
        method: "PATCH",
        body: JSON.stringify({ archived: true }),
      });
      saveDraft(app.drafts, app.selectedThreadId, "");
      stopStream();
      app.selectedThreadId = "";
      app.threadState = createThreadState();
      await loadThreads();
      if (app.summaries[0]) await selectThread(app.summaries[0].id);
      else renderAll();
    } catch (error) {
      showStatus(error.message);
    }
  }

  function openRenameDialog() {
    if (!app.threadState.thread) return;
    dom.renameInput.value = app.threadState.thread.title || "";
    dom.renameDialog.showModal();
    dom.renameInput.focus();
    dom.renameInput.select();
  }

  async function submitRename(event) {
    event.preventDefault();
    const action = event.submitter?.value;
    if (action !== "save") {
      dom.renameDialog.close();
      return;
    }
    const title = dom.renameInput.value.trim();
    if (!title || !app.selectedThreadId) return;
    try {
      const thread = await api(`/v1/threads/${encodeURIComponent(app.selectedThreadId)}`, {
        method: "PATCH",
        body: JSON.stringify({ title }),
      });
      app.threadState.thread = thread;
      dom.renameDialog.close();
      await loadThreads();
      renderHeader();
    } catch (error) {
      showStatus(error.message);
    }
  }

  function resizeComposer() {
    dom.composerInput.style.height = "auto";
    dom.composerInput.style.height = `${Math.min(dom.composerInput.scrollHeight, 220)}px`;
  }

  function closeRailIfNarrow() {
    if (globalThis.matchMedia("(max-width: 800px)").matches) closeRail();
  }

  dom.railOpen.addEventListener("click", openRail);
  dom.railClose.addEventListener("click", closeRail);
  dom.railScrim.addEventListener("click", closeRail);
  dom.newThread.addEventListener("click", () => void openNewThreadDialog());
  dom.newThreadForm.addEventListener("submit", submitNewThread);
  dom.newThreadProvider.addEventListener("change", () => {
    const provider = selectedNewThreadProvider();
    const generation = ++app.newThreadGeneration;
    void loadNewThreadModels(
      provider?.id || "",
      provider?.default_model || "",
      generation,
    );
  });
  dom.newThreadModel.addEventListener("change", syncNewThreadControls);
  dom.newThreadModelInput.addEventListener("input", syncNewThreadControls);
  dom.newThreadCancel.addEventListener("click", () => {
    if (!app.creatingThread) dom.newThreadDialog.close();
  });
  dom.newThreadDialog.addEventListener("cancel", (event) => {
    if (app.creatingThread) event.preventDefault();
  });
  dom.newThreadDialog.addEventListener("keydown", (event) => {
    trapFocusWithin(event, dom.newThreadDialog);
  });
  dom.newThreadDialog.addEventListener("close", () => {
    app.newThreadGeneration += 1;
    app.newThreadLoading = false;
    setNewThreadStatus("");
  });
  dom.rename.addEventListener("click", openRenameDialog);
  dom.archive.addEventListener("click", archiveThread);
  dom.renameForm.addEventListener("submit", submitRename);
  dom.interrupt.addEventListener("click", interruptTurn);
  dom.composer.addEventListener("submit", (event) => {
    event.preventDefault();
    sendMessage();
  });
  dom.composerInput.addEventListener("input", () => {
    saveDraft(app.drafts, app.selectedThreadId, dom.composerInput.value);
    resizeComposer();
    renderComposer();
  });
  dom.composerInput.addEventListener("keydown", (event) => {
    if (!isComposerSubmitKey(event)) return;
    event.preventDefault();
    sendMessage();
  });
  dom.search.addEventListener("input", () => {
    if (app.searchTimer) clearTimeout(app.searchTimer);
    app.searchTimer = setTimeout(() => {
      loadThreads().catch((error) => showStatus(error.message));
      loadSessions().catch(() => {});
    }, 180);
  });
  document.addEventListener("keydown", (event) => {
    if (dom.newThreadDialog.open) return;
    if (trapRailFocus(event)) return;
    if (event.key === "Escape" && narrowRail.matches && dom.shell.classList.contains("rail-visible")) {
      event.preventDefault();
      closeRail();
    }
  });
  narrowRail.addEventListener("change", syncRailAccessibility);
  globalThis.visualViewport?.addEventListener("resize", syncVisualViewport);
  globalThis.visualViewport?.addEventListener("scroll", syncVisualViewport);
  globalThis.addEventListener("resize", syncVisualViewport);
  globalThis.addEventListener("beforeunload", stopStream);

  async function initialize() {
    syncVisualViewport();
    syncRailAccessibility();
    try {
      [app.runtimeInfo, app.workspace] = await Promise.all([
        api("/v1/runtime/info"),
        api("/v1/workspace/status"),
      ]);
      renderRuntimeProvenance(dom.runtimeProvenance, app.runtimeInfo);
      setConnection("ready", "Local runtime connected");
      await loadThreads();
      await loadSessions();
      if (app.summaries[0]) await selectThread(app.summaries[0].id);
      else renderAll();
    } catch (error) {
      setConnection("error", "Runtime connection failed");
      showStatus(error.message);
      renderAll();
    }
  }

  initialize();
}

function basename(path) {
  if (!path) return "";
  const normalized = String(path).replaceAll("\\", "/").replace(/\/$/, "");
  return normalized.split("/").at(-1) || normalized;
}

function humanize(value) {
  if (!value) return "Status";
  return String(value)
    .replaceAll("_", " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

export function modeLabel(mode) {
  if (mode === "agent") return "Work";
  if (mode === "plan") return "Plan";
  if (mode === "operate") return "Operate";
  return humanize(mode || "Runtime default");
}

export function formatRuntimeProvenance(runtimeInfo) {
  const version = String(
    runtimeInfo?.codewhale_version || runtimeInfo?.version || "",
  ).trim() || "version unknown";
  const commit = String(runtimeInfo?.codewhale_commit || "").trim();
  const source = /^[0-9a-f]{40}$/i.test(commit)
    ? commit.slice(0, 12)
    : "source unknown";
  return `${version} · ${source}`;
}

export function renderRuntimeProvenance(element, runtimeInfo) {
  return setSafeText(element, formatRuntimeProvenance(runtimeInfo));
}

function permissionLabel(thread) {
  if (thread.trust_mode) return "Full Access";
  if (thread.auto_approve) return "Auto-Review";
  return "Ask";
}

function relativeTime(value) {
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return "recent";
  const seconds = Math.max(0, Math.round((Date.now() - timestamp) / 1000));
  if (seconds < 60) return "now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}

if (typeof document !== "undefined") {
  startBrowserClient();
}
