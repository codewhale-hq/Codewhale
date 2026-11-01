import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

import {
  NO_TARGET,
  STREAM_EVENT_NAMES,
  answersForUserInput,
  applyRuntimeEvent,
  applySnapshot,
  buildCreateThreadRequest,
  claimInFlight,
  createThreadState,
  createWebSessionFetch,
  createStreamConnector,
  eventStreamUrl,
  formatRuntimeProvenance,
  imageInputPresentation,
  isComposerSubmitKey,
  groupThreadSummaries,
  modeLabel,
  modelOptionLabel,
  newThreadDefaults,
  pendingAttentionCount,
  pendingAttentionLabel,
  providerOptionLabel,
  receiptPresentation,
  recoverSnapshotAndSubscribe,
  renderRuntimeProvenance,
  resolveUserInputTarget,
  restoreDraft,
  runtimeEventContinuity,
  saveDraft,
  sessionTarget,
  setSafeText,
  snapshotThenSubscribe,
  threadTarget,
  threadProviderLabel,
} from "../src/runtime_web/app.mjs";

function snapshot(threadId = "thread-a", latestSeq = 7) {
  return {
    thread: { id: threadId, title: "Test", model: "test", mode: "agent" },
    turns: [{ id: "turn-1", status: "in_progress" }],
    items: [
      {
        id: "item-1",
        turn_id: "turn-1",
        kind: "agent_message",
        status: "in_progress",
        summary: "",
        detail: "Hello",
      },
    ],
    latest_seq: latestSeq,
  };
}

function runtimeEvent(sequence, event, payload = {}, overrides = {}) {
  return {
    schema_version: 1,
    seq: sequence,
    event,
    kind: event,
    thread_id: "thread-a",
    turn_id: "turn-1",
    item_id: null,
    payload,
    ...overrides,
  };
}

function cssDeclarations(styles, selectorPattern) {
  const match = styles.match(new RegExp(`${selectorPattern}\\s*\\{([^}]*)\\}`));
  assert.ok(match, `missing CSS rule matching ${selectorPattern}`);
  return match[1];
}

test("embedded web client uses the Ocean Blue Stage semantic palette", async () => {
  const [styles, html] = await Promise.all([
    readFile(new URL("../src/runtime_web/styles.css", import.meta.url), "utf8"),
    readFile(new URL("../src/runtime_web/index.html", import.meta.url), "utf8"),
  ]);

  for (const token of [
    "--bg: #020711",
    "--sidebar: #050b16",
    "--surface: #0e1a30",
    "--surface-raised: #172945",
    "--stage-surface: #142747",
    "--text: #f6f2e8",
    "--action: #6aaef2",
    "--status-human: #f6c453",
    "--status-live: #4fd1c5",
    "--status-warning: #ff7a59",
    "--status-danger: #ff86b2",
    "--ok: #9bd66f",
    "--radius-control: 6px",
    "--radius-card: 12px",
    "--radius-composer: 16px",
    "--rail: 256px",
  ]) {
    assert.match(styles, new RegExp(token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  }
  assert.match(
    cssDeclarations(styles, "\\.primary-button,\\s*\\.send-button"),
    /background: var\(--action\)/,
  );
  assert.match(
    cssDeclarations(styles, "\\.status-pip\\.running"),
    /background: var\(--live\)/,
  );
  assert.match(
    cssDeclarations(styles, "\\.message\\.user \\.message-body"),
    /background: var\(--plate\)/,
  );
  assert.match(
    cssDeclarations(styles, "\\.attention-card"),
    /border: 1px solid rgba\(246, 196, 83/,
  );
  assert.match(
    cssDeclarations(styles, "\\.status-banner"),
    /color: var\(--warning\)/,
  );
  assert.match(
    cssDeclarations(styles, "\\.connection-dot\\.ready"),
    /background: var\(--ok\)/,
  );
  assert.match(html, /name="theme-color" content="#020711"/);
});

test("embedded web client keeps the CWC stage, transcript, and receipt hierarchy quiet", async () => {
  const [styles, html, source] = await Promise.all([
    readFile(new URL("../src/runtime_web/styles.css", import.meta.url), "utf8"),
    readFile(new URL("../src/runtime_web/index.html", import.meta.url), "utf8"),
    readFile(new URL("../src/runtime_web/app.mjs", import.meta.url), "utf8"),
  ]);

  assert.match(cssDeclarations(styles, "\\.session"), /background: var\(--stage-surface\)/);
  assert.match(cssDeclarations(styles, "\\.transcript"), /background: var\(--stage-surface\)/);
  assert.match(cssDeclarations(styles, "\\.receipt"), /display:\s*flex/);
  assert.match(cssDeclarations(styles, "\\.receipt-dot"), /background: var\(--live\)/);
  assert.match(
    cssDeclarations(styles, "\\.message\\.user \\.message-label"),
    /display:\s*none/,
  );
  assert.match(html, /id="transcript" role="log"[^>]+aria-relevant="additions"/);
  assert.doesNotMatch(html, /id="transcript" role="log"[^>]+aria-relevant="[^"]*text/);
  assert.match(source, /card\.append\(element\("span", "receipt-dot"\)\)/);
});

test("thread rail groups typed pending requests without disturbing server order", async () => {
  const summaries = [
    { id: "newest-recent", pending_attention_count: 0 },
    { id: "newest-needs-you", pending_attention_count: 2 },
    { id: "older-needs-you", pending_attention_count: 1 },
    { id: "older-recent", pending_attention_count: 0 },
  ];
  const groups = groupThreadSummaries(summaries);

  assert.deepEqual(groups.needsYou.map(({ id }) => id), ["newest-needs-you", "older-needs-you"]);
  assert.deepEqual(groups.recent.map(({ id }) => id), ["newest-recent", "older-recent"]);
  assert.deepEqual(summaries.map(({ id }) => id), [
    "newest-recent",
    "newest-needs-you",
    "older-needs-you",
    "older-recent",
  ]);
  assert.equal(pendingAttentionCount({ pending_attention_count: -1 }), 0);
  assert.equal(pendingAttentionLabel({ pending_attention_count: 1 }), "1 item needs your attention");
  assert.equal(pendingAttentionLabel({ pending_attention_count: 2 }), "2 items need your attention");

  const [html, source] = await Promise.all([
    readFile(new URL("../src/runtime_web/index.html", import.meta.url), "utf8"),
    readFile(new URL("../src/runtime_web/app.mjs", import.meta.url), "utf8"),
  ]);
  assert.match(html, /id="thread-list" aria-label="Live threads"/);
  assert.match(source, /group\.setAttribute\("aria-labelledby", headingId\)/);
  assert.match(source, /attention\.setAttribute\("aria-label", pendingAttentionLabel\(summary\)\)/);
});

test("mobile drawer owns focus and background interaction while it is open", async () => {
  const [html, source] = await Promise.all([
    readFile(new URL("../src/runtime_web/index.html", import.meta.url), "utf8"),
    readFile(new URL("../src/runtime_web/app.mjs", import.meta.url), "utf8"),
  ]);

  assert.match(html, /id="rail-open"[^>]+aria-controls="thread-rail"[^>]+aria-expanded="false"/);
  assert.match(html, /id="rail-scrim"[^>]+tabindex="-1" hidden/);
  assert.match(source, /function openRail\(\)[\s\S]*dom\.railClose\.focus/);
  assert.match(source, /dom\.session\.setAttribute\("aria-hidden", "true"\)[\s\S]*setInert\(dom\.session, true\)/);
  assert.match(source, /function closeRail[\s\S]*returnTarget\.focus[\s\S]*applyClosedMobileRailAccessibility/);
  assert.match(source, /function trapFocusWithin[\s\S]*event\.key !== "Tab"[\s\S]*first\.focus/);
  assert.match(source, /function trapRailFocus[\s\S]*trapFocusWithin\(event, dom\.rail\)/);
  assert.match(source, /document\.addEventListener\("keydown", \(event\) => \{\s+if \(dom\.newThreadDialog\.open\) return;\s+if \(trapRailFocus\(event\)\) return;/);
  assert.match(source, /event\.key === "Escape"[\s\S]*closeRail\(\)/);
});

test("mobile viewport and truth controls survive the software keyboard and coarse input", async () => {
  const [styles, source] = await Promise.all([
    readFile(new URL("../src/runtime_web/styles.css", import.meta.url), "utf8"),
    readFile(new URL("../src/runtime_web/app.mjs", import.meta.url), "utf8"),
  ]);

  assert.match(styles, /height: var\(--visual-viewport-height\)/);
  assert.match(source, /globalThis\.visualViewport\?\.addEventListener\("resize", syncVisualViewport\)/);
  assert.match(styles, /@media \(pointer: coarse\)[\s\S]*min-height: 44px/);
  assert.match(
    styles,
    /@media \(max-width: 800px\)[\s\S]*\.composer textarea,[\s\S]*font-size: 16px/,
  );
  assert.match(styles, /@media \(max-width: 430px\)[\s\S]*\.session-facts \{[\s\S]*display: flex/);
  assert.match(styles, /\.session-facts \.fact-chip\[data-fact="workspace"\][\s\S]*display: none/);
  assert.match(source, /chip\.dataset\.fact = String\(label \|\| ""\)\.toLowerCase\(\)/);
});

test("stream reconciliation preserves live controls, disclosures, and selected transcript text", async () => {
  const [html, source] = await Promise.all([
    readFile(new URL("../src/runtime_web/index.html", import.meta.url), "utf8"),
    readFile(new URL("../src/runtime_web/app.mjs", import.meta.url), "utf8"),
  ]);

  assert.match(html, /id="attention" role="region"[^>]+aria-live="assertive"[^>]+aria-relevant="additions"/);
  assert.equal(source.includes("dom.transcript.replaceChildren"), false);
  assert.equal(source.includes("dom.attention.replaceChildren"), false);
  assert.match(source, /function reconcileChildren\(/);
  assert.match(source, /captureTranscriptSelection\(\)[\s\S]*restoreTranscriptSelection\(selection\)/);
  assert.match(source, /card\.dataset\.attentionKey = key/);
  assert.equal(source.includes("focusPendingAttention"), false);
  assert.doesNotMatch(source, /card\.tabIndex = -1/);
});

test("attention requests stay single-flight without stealing the active control", async () => {
  const requests = new Set();
  assert.equal(claimInFlight(requests, "approval:one"), true);
  assert.equal(claimInFlight(requests, "approval:one"), false);
  requests.delete("approval:one");
  assert.equal(claimInFlight(requests, "approval:one"), true);

  const source = await readFile(
    new URL("../src/runtime_web/app.mjs", import.meta.url),
    "utf8",
  );
  assert.match(source, /if \(!claimInFlight\(app\.inFlightActions, action\)\) return;/);
  assert.match(source, /setAttentionCardBusy\(action, true\)/);
  assert.match(source, /finally \{\s+app\.inFlightActions\.delete\(action\);\s+setAttentionCardBusy\(action, false\);/);
});

test("degraded workflow receipts surface rejected dispatches as attention", () => {
  const detail = JSON.stringify({
    status: "degraded",
    dispatch_failure_count: 2,
    dispatch_failures: [{ label: "review", message: "profile unavailable" }],
  });
  assert.deepEqual(receiptPresentation({
    kind: "tool_call",
    status: "completed",
    summary: "workflow: degraded",
    detail,
    metadata: { status: "degraded", dispatch_failure_count: 2 },
  }), {
    label: "Workflow · Needs attention",
    summary: "2 task dispatches were rejected",
    raw: `workflow: degraded\n\n${detail}`,
    failed: true,
  });
});

test("rail New thread cannot paint over the session fact chips", async () => {
  const styles = await readFile(
    new URL("../src/runtime_web/styles.css", import.meta.url),
    "utf8",
  );
  assert.match(cssDeclarations(styles, "\\.rail"), /overflow:\s*hidden/);
  assert.match(cssDeclarations(styles, "\\.new-thread"), /max-width:\s*100%/);
  assert.match(cssDeclarations(styles, "\\.session-header"), /overflow:\s*hidden/);
  assert.match(cssDeclarations(styles, "\\.session-facts"), /flex-wrap:\s*nowrap/);
});

test("production shell keeps readable type, controls, focus, and motion contracts", async () => {
  const [styles, html] = await Promise.all([
    readFile(new URL("../src/runtime_web/styles.css", import.meta.url), "utf8"),
    readFile(new URL("../src/runtime_web/index.html", import.meta.url), "utf8"),
  ]);

  assert.match(cssDeclarations(styles, "\\.thread-row"), /min-height:\s*62px/);
  assert.match(cssDeclarations(styles, "\\.thread-title"), /font-size:\s*14px/);
  assert.match(cssDeclarations(styles, "\\.message-body"), /font-size:\s*15\.5px/);
  assert.match(cssDeclarations(styles, "\\.composer textarea"), /font-size:\s*15\.5px/);
  assert.match(cssDeclarations(styles, "\\.composer textarea"), /max-height:\s*220px/);
  assert.match(
    styles,
    /\.primary-button,[\s\S]*?\.icon-button\s*\{[\s\S]*?min-height:\s*36px/,
  );
  assert.match(
    styles,
    /button:focus-visible,[\s\S]*?outline:\s*3px solid var\(--action\)/,
  );
  assert.match(
    styles,
    /@media \(prefers-reduced-motion: reduce\)[\s\S]*scroll-behavior: auto !important/,
  );
  assert.match(html, /Enter send · Shift\+Enter newline/);
});

test("composer Enter sends without interrupting newlines or IME composition", () => {
  assert.equal(isComposerSubmitKey({ key: "Enter" }), true);
  assert.equal(isComposerSubmitKey({ key: "Enter", metaKey: true }), true);
  assert.equal(isComposerSubmitKey({ key: "Enter", ctrlKey: true }), true);
  assert.equal(isComposerSubmitKey({ key: "Enter", shiftKey: true }), false);
  assert.equal(isComposerSubmitKey({ key: "Enter", isComposing: true }), false);
  assert.equal(isComposerSubmitKey({ key: "a" }), false);
});

test("new thread selection keeps provider and model scoped to the create request", () => {
  const catalog = {
    current: "deepseek",
    providers: [
      { id: "openai", default_model: "gpt-5.6" },
      { id: "deepseek", default_model: "deepseek-v4-pro" },
    ],
  };
  assert.deepEqual(newThreadDefaults(catalog), {
    providerId: "deepseek",
    modelProviderId: "",
    model: "deepseek-v4-pro",
  });
  assert.deepEqual(
    buildCreateThreadRequest(" deepseek ", " deepseek-v4-flash-vision-exp "),
    {
      model_provider: "deepseek",
      model: "deepseek-v4-flash-vision-exp",
    },
  );
  assert.throws(() => buildCreateThreadRequest("deepseek", ""), /provider and a model/);

  const namedCustom = {
    current: "custom",
    providers: [{
      id: "custom",
      model_provider_id: "lm-studio",
      default_model: "local-vision-model",
    }],
  };
  assert.deepEqual(newThreadDefaults(namedCustom), {
    providerId: "custom",
    modelProviderId: "lm-studio",
    model: "local-vision-model",
  });
  assert.deepEqual(
    buildCreateThreadRequest("custom", "local-vision-model", "lm-studio"),
    {
      model_provider: "custom",
      model: "local-vision-model",
      model_provider_id: "lm-studio",
    },
  );
  assert.equal(
    providerOptionLabel({
      id: "custom",
      display_name: "Custom",
      model_provider_id: "lm-studio",
    }),
    "Custom · lm-studio",
  );
  assert.equal(threadProviderLabel({
    model_provider: "custom",
    model_provider_id: "lm-studio",
  }), "lm-studio");
  assert.equal(threadProviderLabel({ model_provider: "deepseek" }), "deepseek");
});

test("thread facts keep exact named provider identity visible after creation", async () => {
  const source = await readFile(
    new URL("../src/runtime_web/app.mjs", import.meta.url),
    "utf8",
  );
  assert.match(source, /const provider = threadProviderLabel\(thread\)/);
  assert.match(source, /factChip\("Provider", provider\)/);
});

test("composer send guard survives rerenders until the request settles", async () => {
  const source = await readFile(
    new URL("../src/runtime_web/app.mjs", import.meta.url),
    "utf8",
  );
  assert.match(source, /const sending = app\.inFlightActions\.has\(composerSendAction\)/);
  assert.match(source, /dom\.composerInput\.disabled = sending \|\| !ready/);
  assert.match(source, /dom\.send\.disabled = sending \|\| !ready/);
  assert.match(source, /if \(!claimInFlight\(app\.inFlightActions, composerSendAction\)\) return;/);
  assert.match(source, /finally \{\s+app\.inFlightActions\.delete\(composerSendAction\);\s+renderComposer\(\);/);
});

test("new thread dialog labels exact vision capability without exposing attachments", async () => {
  const [html, source] = await Promise.all([
    readFile(new URL("../src/runtime_web/index.html", import.meta.url), "utf8"),
    readFile(new URL("../src/runtime_web/app.mjs", import.meta.url), "utf8"),
  ]);
  const vision = { id: "deepseek-v4-flash-vision-exp", image_input: "supported" };
  assert.equal(modelOptionLabel(vision), "deepseek-v4-flash-vision-exp · Vision");
  assert.equal(imageInputPresentation("supported").label, "Vision");
  assert.equal(imageInputPresentation("unsupported").label, "Text only");
  assert.equal(imageInputPresentation("unknown").state, "unknown");

  assert.match(html, /id="new-thread-dialog"[^>]+tabindex="-1"[^>]+aria-labelledby="new-thread-title"/);
  assert.match(html, /id="new-thread-cancel"[^>]+autofocus/);
  assert.match(html, /id="new-thread-provider" required disabled/);
  assert.match(html, /id="new-thread-model" required disabled/);
  assert.match(html, /does not change your Runtime defaults/);
  assert.doesNotMatch(html, /type="file"/);
  assert.match(source, /api\("\/v1\/providers"\)/);
  // The dialog loads the catalog through the bounded, paginated collector
  // keyed by provider.id; the wire endpoint stays /v1/providers/<id>/models.
  assert.match(source, /collectProviderModelPages\(provider\.id/);
  assert.match(source, /\/v1\/providers\/\$\{encodeURIComponent\(provider\)\}\/models\?\$\{query\.toString\(\)\}/);
  assert.match(source, /body: JSON\.stringify\(request\)/);
  assert.match(source, /function trapFocusWithin\(event, container\)/);
  assert.match(source, /dom\.newThreadCancel\.focus\(\{ preventScroll: true \}\)/);
  assert.match(source, /trapFocusWithin\(event, dom\.newThreadDialog\)/);
  assert.doesNotMatch(source, /\/v1\/providers\/[^`"']+\/switch/);
});

test("uses the v0.9.6 Work vocabulary for the agent wire mode", () => {
  assert.equal(modeLabel("agent"), "Work");
  assert.equal(modeLabel("plan"), "Plan");
  assert.equal(modeLabel("operate"), "Operate");
});

test("formats and renders exact Runtime build provenance with honest fallbacks", () => {
  const exactCommit = "abcdef0123456789abcdef0123456789abcdef01";
  const stamped = {
    codewhale_version: "0.9.6",
    codewhale_commit: exactCommit,
  };
  assert.equal(formatRuntimeProvenance(stamped), "0.9.6 · abcdef012345");

  const rendered = { textContent: "" };
  renderRuntimeProvenance(rendered, stamped);
  assert.equal(rendered.textContent, "0.9.6 · abcdef012345");

  assert.equal(
    formatRuntimeProvenance({ codewhale_version: "0.9.6", codewhale_commit: "unknown" }),
    "0.9.6 · source unknown",
  );
  assert.equal(
    formatRuntimeProvenance({ version: "0.9.6", codewhale_commit: "too-short" }),
    "0.9.6 · source unknown",
  );
  assert.equal(formatRuntimeProvenance(null), "version unknown · source unknown");
});

test("loads a consistent snapshot before subscribing from latest_seq", async () => {
  const state = createThreadState("thread-a");
  const order = [];
  const subscribed = await snapshotThenSubscribe({
    state,
    threadId: "thread-a",
    loadSnapshot: async () => {
      order.push("snapshot");
      return snapshot("thread-a", 42);
    },
    subscribe: (threadId, sequence) => order.push(`subscribe:${threadId}:${sequence}`),
  });

  assert.equal(subscribed, true);
  assert.deepEqual(order, ["snapshot", "subscribe:thread-a:42"]);
  assert.equal(state.latestSeq, 42);
});

test("snapshot recovery waits for the replacement stream to open", async () => {
  const state = createThreadState("thread-a");
  let finishOpening;
  let settled = false;
  const opening = new Promise((resolve) => {
    finishOpening = resolve;
  });
  const recovery = snapshotThenSubscribe({
    state,
    threadId: "thread-a",
    loadSnapshot: async () => snapshot("thread-a", 43),
    subscribe: () => opening,
  }).then((result) => {
    settled = true;
    return result;
  });

  await Promise.resolve();
  assert.equal(settled, false, "snapshot success alone must not finish recovery");
  finishOpening();
  assert.equal(await recovery, true);
  assert.equal(settled, true);
});

test("a failed replacement stream keeps the gap until a later stream opens", async () => {
  const state = createThreadState("thread-a");
  let gap = true;
  let attempts = 0;
  const recover = () => recoverSnapshotAndSubscribe({
    state,
    threadId: "thread-a",
    loadSnapshot: async () => snapshot("thread-a", 44 + attempts),
    subscribe: async () => {
      attempts += 1;
      if (attempts === 1) throw new Error("replacement stream did not reopen");
    },
  }, () => {
    gap = false;
  });

  await assert.rejects(recover(), /did not reopen/);
  assert.equal(gap, true, "snapshot success must not hide a failed stream handshake");
  assert.equal(await recover(), true);
  assert.equal(gap, false, "a later snapshot plus open stream clears the gap");
});

test("drops a stale snapshot selection without opening an event stream", async () => {
  const state = createThreadState("thread-a");
  let current = true;
  let subscribed = false;
  const result = await snapshotThenSubscribe({
    state,
    threadId: "thread-a",
    loadSnapshot: async () => {
      current = false;
      return snapshot();
    },
    subscribe: () => {
      subscribed = true;
    },
    isCurrent: () => current,
  });
  assert.equal(result, false);
  assert.equal(subscribed, false);
});

test("reconnect cursor advances monotonically and duplicate or stale-thread events are ignored", () => {
  const state = createThreadState("thread-a");
  assert.equal(applySnapshot(state, snapshot("thread-a", 7)), true);

  assert.equal(
    applyRuntimeEvent(
      state,
      runtimeEvent(8, "item.delta", { delta: " world", kind: "agent_message" }, { item_id: "item-1" }),
    ),
    true,
  );
  assert.equal(
    applyRuntimeEvent(
      state,
      runtimeEvent(8, "item.delta", { delta: " duplicate", kind: "agent_message" }, { item_id: "item-1" }),
    ),
    false,
  );
  assert.equal(
    applyRuntimeEvent(state, runtimeEvent(99, "turn.completed", {}, { thread_id: "thread-b" })),
    false,
  );
  assert.equal(state.items.get("item-1").detail, "Hello world");
  assert.equal(state.latestSeq, 8);
  assert.equal(eventStreamUrl("thread-a", state.latestSeq), "/v1/threads/thread-a/events?since_seq=8");
});

test("uses the stream predecessor cursor to detect real gaps without assuming global sequences are contiguous", () => {
  const state = createThreadState("thread-a");
  applySnapshot(state, snapshot("thread-a", 7));

  const interleaved = runtimeEvent(
    12,
    "item.delta",
    { delta: " after other threads", kind: "agent_message" },
    { item_id: "item-1", previous_seq: 7 },
  );
  assert.equal(runtimeEventContinuity(state, interleaved), "next");
  assert.equal(applyRuntimeEvent(state, interleaved), true);
  assert.equal(state.latestSeq, 12);

  const gap = runtimeEvent(
    15,
    "approval.required",
    { approval_id: "approval-missed", tool_name: "exec_shell" },
    { previous_seq: 14 },
  );
  assert.equal(runtimeEventContinuity(state, gap), "gap");
  assert.equal(applyRuntimeEvent(state, gap), false);
  assert.equal(state.latestSeq, 12);
  assert.equal(state.approvals.has("approval-missed"), false);
});

test("registers the full emitted Runtime vocabulary and advances continuity for every event", async () => {
  const runtimeSource = (
    await Promise.all(
      ["../src/runtime_threads.rs", "../src/runtime_threads/manager.rs"].map((path) =>
        readFile(new URL(path, import.meta.url), "utf8"),
      ),
    )
  ).join("\n");
  const emittedNames = new Set(
    [...runtimeSource.matchAll(
      /"((?:thread|turn|item|approval|user_input|sandbox|agent|tool_call)\.[a-z_]+)"/g,
    )].map((match) => match[1]),
  );
  assert.deepEqual(new Set(STREAM_EVENT_NAMES), emittedNames);
  assert.equal(STREAM_EVENT_NAMES.includes("thread.created"), false);

  const state = createThreadState("thread-a");
  applySnapshot(state, snapshot("thread-a", 7));
  let previousSeq = 7;
  for (const eventName of STREAM_EVENT_NAMES) {
    const sequence = previousSeq + 2;
    const turnBefore = state.turns.get("turn-1");
    const payload = eventName === "turn.usage"
      ? { usage: { input_tokens: 100, output_tokens: 20 } }
      : {};
    const envelope = runtimeEvent(sequence, eventName, payload, { previous_seq: previousSeq });
    assert.equal(runtimeEventContinuity(state, envelope), "next", eventName);
    assert.equal(applyRuntimeEvent(state, envelope), true, eventName);
    assert.equal(state.latestSeq, sequence, eventName);
    if (eventName === "turn.usage") {
      // Request diagnostics advance continuity; only the settled turn owns totals.
      assert.equal(state.turns.get("turn-1"), turnBefore);
      assert.equal(applyRuntimeEvent(state, envelope), false);
    }
    previousSeq = sequence;
  }
  const settledTurn = {
    id: "turn-1",
    status: "completed",
    usage: { input_tokens: 300, output_tokens: 60 },
  };
  assert.equal(applyRuntimeEvent(state, runtimeEvent(
    previousSeq + 1,
    "turn.completed",
    { turn: settledTurn },
    { previous_seq: previousSeq },
  )), true);
  assert.deepEqual(state.turns.get("turn-1"), settledTurn);
});

test("gap recovery snapshot restores approval and user-input attention before resubscribing", async () => {
  const state = createThreadState("thread-a");
  applySnapshot(state, snapshot("thread-a", 7));
  const subscriptions = [];

  const recovered = await snapshotThenSubscribe({
    state,
    threadId: "thread-a",
    loadSnapshot: async () => ({
      ...snapshot("thread-a", 15),
      pending_approvals: [{
        id: "approval-recovered",
        turn_id: "turn-1",
        tool_name: "exec_command",
        description: "Run a local check",
      }],
      pending_user_inputs: [{
        id: "input-recovered",
        turn_id: "turn-1",
        request: { questions: [{ id: "choice", question: "Continue?", options: [] }] },
      }],
      pending_dynamic_tool_calls: [{
        thread_id: "thread-a",
        turn_id: "turn-1",
        call_id: "call-recovered",
        namespace: "bench",
        tool: "lookup",
        arguments: { id: "7" },
      }],
    }),
    subscribe: (threadId, sequence) => subscriptions.push([threadId, sequence]),
  });

  assert.equal(recovered, true);
  assert.equal(state.approvals.size, 1);
  assert.equal(state.approvals.has("approval-recovered"), true);
  assert.equal(state.userInputs.size, 1);
  assert.equal(state.userInputs.has("input-recovered"), true);
  assert.equal(state.dynamicToolCalls.size, 1);
  assert.equal(state.dynamicToolCalls.get("call-recovered").tool, "lookup");
  assert.deepEqual(subscriptions, [["thread-a", 15]]);

  const duplicate = runtimeEvent(
    15,
    "approval.required",
    { approval_id: "approval-recovered", tool_name: "exec_command" },
    { previous_seq: 14 },
  );
  assert.equal(applyRuntimeEvent(state, duplicate), false);
  assert.equal(state.approvals.size, 1);
});

test("browser clears its surfaced gap only after a replacement snapshot subscribes", async () => {
  const source = await readFile(new URL("../src/runtime_web/app.mjs", import.meta.url), "utf8");
  assert.match(
    source,
    /async function recoverProjection[\s\S]*?connectStream\(id, sequence, generation, true\)/,
  );
  assert.match(
    source,
    /async function recoverProjection[\s\S]*?recoverSnapshotAndSubscribe\([\s\S]*?app\.streamGap = false;[\s\S]*?if \(!subscribed\) return;\s+renderAll\(\);/,
  );
});

test("user-input answers stay bound to the selected live thread and pending request", () => {
  const state = createThreadState("thread-a");
  state.userInputs.set("input-1", {});

  assert.deepEqual(
    resolveUserInputTarget("input-1", threadTarget("thread-a"), state),
    { ok: true, threadId: "thread-a", inputId: "input-1" },
  );
  assert.deepEqual(
    resolveUserInputTarget("input-1", sessionTarget("session-a"), state),
    { ok: false, reason: "session-not-live" },
  );
  assert.deepEqual(
    resolveUserInputTarget("input-1", NO_TARGET, state),
    { ok: false, reason: "no-target" },
  );
  assert.deepEqual(
    resolveUserInputTarget("input-2", threadTarget("thread-a"), state),
    { ok: false, reason: "stale-user-input" },
  );
  assert.deepEqual(
    resolveUserInputTarget("input-1", threadTarget("thread-b"), state),
    { ok: false, reason: "stale-target" },
  );
});

test("user-input payloads preserve TUI custom-answer parity and single-select cardinality", () => {
  const single = {
    questions: [{
      id: "path",
      header: "Path",
      question: "Which path?",
      options: [{ label: "A" }, { label: "B" }],
      allow_free_text: false,
      multi_select: false,
    }],
  };
  assert.deepEqual(answersForUserInput(single, {}, { path: "A different path" }), {
    ok: true,
    answers: [{ id: "path", label: "Other", value: "A different path" }],
  });
  assert.equal(
    answersForUserInput(single, { path: ["A"] }, { path: "also B" }).reason,
    "multiple-answers",
  );
  assert.equal(
    answersForUserInput(single, { path: ["forged"] }, {}).reason,
    "invalid-option",
  );
  assert.equal(answersForUserInput(single, {}, {}).reason, "missing-answer");

  const multi = {
    questions: [{
      ...single.questions[0],
      id: "checks",
      multi_select: true,
    }],
  };
  assert.deepEqual(
    answersForUserInput(multi, { checks: ["A", "B"] }, { checks: "C" }),
    {
      ok: true,
      answers: [
        { id: "checks", label: "A", value: "A" },
        { id: "checks", label: "B", value: "B" },
        { id: "checks", label: "Other", value: "C" },
      ],
    },
  );
});

test("assembles deltas and replaces the live item with its settled receipt", () => {
  const state = createThreadState("thread-a");
  applySnapshot(state, { ...snapshot(), items: [], latest_seq: 1 });
  applyRuntimeEvent(
    state,
    runtimeEvent(2, "item.delta", { delta: "one", kind: "agent_message" }, { item_id: "item-new" }),
  );
  applyRuntimeEvent(
    state,
    runtimeEvent(3, "item.delta", { delta: " two", kind: "agent_message" }, { item_id: "item-new" }),
  );
  assert.equal(state.items.get("item-new").detail, "one two");

  applyRuntimeEvent(
    state,
    runtimeEvent(
      4,
      "item.completed",
      {
        item: {
          id: "item-new",
          turn_id: "turn-1",
          kind: "agent_message",
          status: "completed",
          summary: "one two",
          detail: "one two",
        },
      },
      { item_id: "item-new" },
    ),
  );
  assert.equal(state.items.get("item-new").status, "completed");
  assert.deepEqual(state.itemOrder, ["item-new"]);

  applyRuntimeEvent(
    state,
    runtimeEvent(5, "item.delta", { delta: "partial", kind: "tool_call" }, { item_id: "item-stop" }),
  );
  applyRuntimeEvent(
    state,
    runtimeEvent(
      6,
      "item.interrupted",
      {
        item: {
          id: "item-stop",
          turn_id: "turn-1",
          kind: "tool_call",
          status: "interrupted",
          summary: "Interrupted",
          detail: "partial",
        },
      },
      { item_id: "item-stop" },
    ),
  );
  assert.equal(state.items.get("item-stop").status, "interrupted");

  applyRuntimeEvent(
    state,
    runtimeEvent(
      7,
      "item.canceled",
      {
        item: {
          id: "item-compact",
          turn_id: "turn-1",
          kind: "compaction",
          status: "canceled",
          summary: "Compaction canceled",
          detail: "Compaction canceled",
        },
      },
      { item_id: "item-compact" },
    ),
  );
  assert.equal(state.items.get("item-compact").status, "canceled");
  assert.deepEqual(state.itemOrder, ["item-new", "item-stop", "item-compact"]);
});

test("projects agent lifecycle receipts live and settles them without a snapshot reload", () => {
  const state = createThreadState("thread-a");
  applySnapshot(state, { ...snapshot(), items: [], latest_seq: 1 });

  const agentItem = (status, summary) => ({
    id: "item-agent",
    turn_id: "turn-1",
    kind: "status",
    status,
    summary,
    detail: summary,
  });
  applyRuntimeEvent(
    state,
    runtimeEvent(2, "agent.spawned", { item: agentItem("in_progress", "Agent spawned") }),
  );
  assert.equal(state.items.get("item-agent").status, "in_progress");
  assert.deepEqual(state.itemOrder, ["item-agent"]);

  applyRuntimeEvent(
    state,
    runtimeEvent(3, "agent.progress", { item: agentItem("in_progress", "Agent checking") }),
  );
  applyRuntimeEvent(
    state,
    runtimeEvent(4, "agent.completed", { item: agentItem("completed", "Agent completed") }),
  );
  assert.equal(state.items.get("item-agent").status, "completed");
  assert.equal(state.items.get("item-agent").summary, "Agent completed");
  assert.deepEqual(state.itemOrder, ["item-agent"]);

  applyRuntimeEvent(
    state,
    runtimeEvent(5, "agent.list", {
      item: {
        id: "item-agent-list",
        turn_id: "turn-1",
        kind: "status",
        status: "completed",
        summary: "Agent list refreshed",
        detail: "Agent list refreshed",
      },
    }),
  );
  assert.equal(state.items.get("item-agent-list").status, "completed");
  assert.deepEqual(state.itemOrder, ["item-agent", "item-agent-list"]);
  assert.equal(state.latestSeq, 5);
});

test("tracks approval and user-input attention until each is resolved", () => {
  const state = createThreadState("thread-a");
  applySnapshot(state, snapshot());
  applyRuntimeEvent(
    state,
    runtimeEvent(8, "approval.required", { approval_id: "approval-1", tool_name: "exec_shell" }),
  );
  applyRuntimeEvent(
    state,
    runtimeEvent(9, "user_input.required", {
      id: "input-1",
      request: { questions: [{ id: "choice", question: "Choose?", options: [] }] },
    }),
  );
  assert.equal(state.approvals.has("approval-1"), true);
  assert.equal(state.userInputs.has("input-1"), true);

  applyRuntimeEvent(
    state,
    runtimeEvent(10, "approval.decided", { approval_id: "approval-1", decision: "allow" }),
  );
  assert.equal(state.approvals.has("approval-1"), false);
  assert.equal(state.userInputs.has("input-1"), true);

  applyRuntimeEvent(
    state,
    runtimeEvent(11, "user_input.answered", { input_id: "input-1" }),
  );
  assert.equal(state.userInputs.has("input-1"), false);
});

test("hydrates pending attention from a reload snapshot and clears cancellation events", () => {
  const state = createThreadState("thread-a");
  const detail = {
    ...snapshot(),
    pending_approvals: [{
      id: "approval-reload",
      turn_id: "turn-1",
      tool_name: "exec_command",
      description: "Run a local check",
    }],
    pending_user_inputs: [{
      id: "input-reload",
      turn_id: "turn-1",
      request: { questions: [{ id: "choice", question: "Continue?", options: [] }] },
    }],
  };

  assert.equal(applySnapshot(state, detail), true);
  assert.equal(state.approvals.get("approval-reload").tool_name, "exec_command");
  assert.equal(state.userInputs.get("input-reload").turn_id, "turn-1");

  applyRuntimeEvent(
    state,
    runtimeEvent(8, "user_input.canceled", { id: "input-reload", terminal: true }),
  );
  assert.equal(state.userInputs.has("input-reload"), false);
});

test("turn completion defensively clears attention owned by that turn", () => {
  const state = createThreadState("thread-a");
  assert.equal(applySnapshot(state, {
    ...snapshot(),
    pending_approvals: [{ id: "approval-terminal", turn_id: "turn-1" }],
    pending_user_inputs: [{ id: "input-terminal", turn_id: "turn-1", request: { questions: [] } }],
    pending_dynamic_tool_calls: [{ call_id: "call-terminal", turn_id: "turn-1", tool: "lookup" }],
  }), true);
  state.approvals.set("approval-other", { id: "approval-other", turn_id: "turn-other" });
  state.userInputs.set("input-other", { id: "input-other", turn_id: "turn-other" });
  state.dynamicToolCalls.set("call-other", { call_id: "call-other", turn_id: "turn-other" });

  assert.equal(applyRuntimeEvent(
    state,
    runtimeEvent(8, "turn.completed", { turn: { id: "turn-1", status: "completed" } }),
  ), true);
  assert.equal(state.approvals.has("approval-terminal"), false);
  assert.equal(state.userInputs.has("input-terminal"), false);
  assert.equal(state.dynamicToolCalls.has("call-terminal"), false);
  assert.equal(state.approvals.has("approval-other"), true);
  assert.equal(state.userInputs.has("input-other"), true);
  assert.equal(state.dynamicToolCalls.has("call-other"), true);
});

test("dynamic tool calls hydrate and disappear exactly once across terminal variants", () => {
  const state = createThreadState("thread-a");
  assert.equal(applySnapshot(state, {
    ...snapshot(),
    pending_dynamic_tool_calls: [{
      thread_id: "thread-a",
      turn_id: "turn-1",
      call_id: "call-snapshot",
      tool: "snapshot_lookup",
      arguments: { id: "snapshot" },
    }],
  }), true);
  assert.equal(state.dynamicToolCalls.get("call-snapshot").tool, "snapshot_lookup");

  assert.equal(applyRuntimeEvent(
    state,
    runtimeEvent(8, "tool_call.requested", {
      thread_id: "thread-a",
      turn_id: "turn-1",
      call_id: "call-live",
      tool: "live_lookup",
      arguments: { id: "live" },
    }),
  ), true);
  assert.equal(state.dynamicToolCalls.size, 2);

  assert.equal(applyRuntimeEvent(
    state,
    runtimeEvent(9, "tool_call.resolved", { call_id: "call-snapshot", status: "resolved" }),
  ), true);
  assert.equal(state.dynamicToolCalls.has("call-snapshot"), false);
  assert.equal(applyRuntimeEvent(
    state,
    runtimeEvent(9, "tool_call.resolved", { call_id: "call-snapshot", status: "resolved" }),
  ), false);
  assert.equal(state.dynamicToolCalls.size, 1);

  assert.equal(applyRuntimeEvent(
    state,
    runtimeEvent(10, "tool_call.canceled", { call_id: "call-live", status: "canceled" }),
  ), true);
  assert.equal(state.dynamicToolCalls.size, 0);

  assert.equal(applyRuntimeEvent(
    state,
    runtimeEvent(11, "tool_call.requested", {
      call_id: "call-timeout",
      tool: "slow_lookup",
      arguments: {},
    }),
  ), true);
  assert.equal(state.dynamicToolCalls.has("call-timeout"), true);
  assert.equal(applyRuntimeEvent(
    state,
    runtimeEvent(12, "tool_call.timeout", { call_id: "call-timeout", status: "timeout" }),
  ), true);
  assert.equal(state.dynamicToolCalls.size, 0);
});

test("preserves drafts per thread without browser storage", () => {
  const drafts = new Map();
  saveDraft(drafts, "thread-a", "draft A");
  saveDraft(drafts, "thread-b", "draft B");
  assert.equal(restoreDraft(drafts, "thread-a"), "draft A");
  assert.equal(restoreDraft(drafts, "thread-b"), "draft B");
  saveDraft(drafts, "thread-a", "");
  assert.equal(restoreDraft(drafts, "thread-a"), "");
});

test("renders hostile Runtime text only through the textContent sink", async () => {
  const hostile = `<img src=x onerror=alert(1)><script>alert(2)</script>`;
  const fakeElement = { textContent: "" };
  setSafeText(fakeElement, hostile);
  assert.equal(fakeElement.textContent, hostile);

  const source = await readFile(new URL("../src/runtime_web/app.mjs", import.meta.url), "utf8");
  assert.equal(source.includes("inner" + "HTML"), false);
  assert.equal(source.includes("insertAdjacent" + "HTML"), false);
  assert.equal(source.includes("local" + "Storage"), false);
  assert.equal(source.includes("codewhale_runtime_token"), false);
});

test("web session fetch keeps request proof outside cookies and strips bootstrap fragments", async () => {
  const stored = new Map();
  const storage = () => ({ setItem: (key, value) => stored.set(key, value), getItem: (key) => stored.get(key) });
  const paths = [];
  const calls = [];
  const history = { replaceState: (_state, _title, path) => paths.push(path) };
  const fetch = async (path, options) => { calls.push({ path, options }); return { ok: true }; };
  const request = createWebSessionFetch({
    location: { hash: "#p=proof-one", pathname: "/", search: "?view=threads" }, history, storage, fetch,
  });
  assert.deepEqual(paths, ["/?view=threads"]);
  await request("/v1/threads", { method: "POST", body: "{}" });
  await request("/__codewhale/web/stream-ticket", { method: "POST" });
  const reloaded = createWebSessionFetch({ location: { hash: "" }, history, storage, fetch });
  await reloaded("/v1/threads");
  for (const { options } of calls) {
    assert.equal(options.headers.get("x-codewhale-web-request"), "proof-one");
    assert.equal(options.headers.has("cookie"), false);
    assert.equal(options.credentials, "same-origin");
    assert.equal(options.cache, "no-store");
  }
  assert.equal(calls[0].options.headers.get("content-type"), "application/json");
  assert.equal(eventStreamUrl("thread-a", 4, "ticket-one"), "/v1/threads/thread-a/events?since_seq=4&web_stream_ticket=ticket-one");

  const memoryOnly = createWebSessionFetch({
    location: { hash: "#p=memory-proof", pathname: "/", search: "" }, history,
    storage: () => { throw new Error("storage unavailable"); }, fetch,
  });
  await memoryOnly("/v1/threads");
  assert.equal(calls.at(-1).options.headers.get("x-codewhale-web-request"), "memory-proof");
});

test("web session uses only its fragment or this tab's stored proof", async () => {
  for (const storedProof of [null, "this-tab-proof", "unavailable"]) {
    const stored = new Map([["codewhale_web_request_proof", storedProof]]);
    const calls = [];
    const request = createWebSessionFetch({
      location: { hash: "" },
      history: { replaceState: () => assert.fail("no fragment to clear") },
      storage: () => {
        if (storedProof === "unavailable") throw new Error("storage unavailable");
        return { getItem: (key) => stored.get(key), setItem: (key, value) => stored.set(key, value) };
      },
      fetch: async (_path, options) => { calls.push(options); return { ok: true }; },
    });
    await request("/v1/threads");
    await request("/__codewhale/web/stream-ticket", { method: "POST" });
    assert.equal(calls.length, 2);
    const expected = storedProof === "this-tab-proof" ? "this-tab-proof" : "";
    for (const options of calls) {
      assert.equal(options.headers.get("x-codewhale-web-request"), expected);
    }
  }
});

function streamHarness(api) {
  const app = {
    generation: 1, selectedThreadId: "thread-a", threadState: { latestSeq: 4 },
    stream: null, streamOpenCancel: null, reconnectTimer: null,
  };
  const streams = [];
  const timers = new Map();
  const statuses = [];
  let nextTimer = 0;
  class EventSource {
    constructor(url) { this.url = url; this.closed = false; streams.push(this); }
    close() { this.closed = true; }
    addEventListener() {}
  }
  const connector = createStreamConnector({
    app, api, EventSource, receive: () => {}, setConnection: () => {},
    showStatus: (message) => statuses.push(message),
    setTimeout: (callback, delay) => { timers.set(++nextTimer, { callback, delay }); return nextTimer; },
    clearTimeout: (id) => timers.delete(id),
  });
  return {
    ...connector, app, streams, timers, statuses,
    tick: async () => {
      assert.equal(timers.size, 1, "exactly one reconnect should be pending");
      const [id, { callback, delay }] = timers.entries().next().value;
      timers.delete(id);
      callback();
      await new Promise((resolve) => setImmediate(resolve));
      return delay;
    },
  };
}

test("stream retries ticket network and 5xx failures with capped backoff and resumes the latest cursor", async () => {
  let requests = 0;
  const h = streamHarness(async () => {
    requests += 1;
    if (requests === 2) throw new TypeError("network unavailable");
    if (requests > 2 && requests < 10) throw Object.assign(new Error("temporarily unavailable"), { status: 503 });
    return { stream_ticket: `ticket-${requests}` };
  });
  await h.connectStream("thread-a", 4, 1);
  h.streams[0].onopen();
  h.streams[0].onerror();
  assert.equal(h.streams[0].closed, true);
  h.app.threadState.latestSeq = 9;
  const delays = [];
  while (h.streams.length === 1) delays.push(await h.tick());
  assert.deepEqual(delays, [900, 1800, 3600, 7200, 14400, 28800, 30000, 30000, 30000]);
  assert.match(h.streams[1].url, /since_seq=9&web_stream_ticket=ticket-10$/);
  h.streams[1].onopen();
  h.streams[1].onerror();
  assert.equal(await h.tick(), 900, "opening a stream resets backoff");
  h.stopStream();
  assert.equal(h.timers.size, 0);
});

test("stream stops ticket retries on 401 and 403", async () => {
  for (const status of [401, 403]) {
    let requests = 0;
    const h = streamHarness(async () => {
      if (++requests === 1) return { stream_ticket: "first" };
      throw Object.assign(new Error("session ended"), { status });
    });
    await h.connectStream("thread-a", 4, 1);
    h.app.stream.onerror();
    await h.tick();
    assert.equal(h.timers.size, 0);
    assert.equal(h.app.stream, null);
    assert.deepEqual(h.statuses, ["session ended"]);
  }
});

test("stream ignores superseded ticket completions and failures, including stopped connections", async () => {
  const pending = [];
  const h = streamHarness(() => new Promise((resolve, reject) => pending.push({ resolve, reject })));
  const first = h.connectStream("thread-a", 4, 1);
  const second = h.connectStream("thread-a", 4, 1);
  pending[1].resolve({ stream_ticket: "current" });
  await second;
  pending[0].resolve({ stream_ticket: "stale" });
  await first;
  assert.equal(h.streams.length, 1, "a late ticket cannot create an orphan stream");
  assert.match(h.app.stream.url, /current$/);
  const third = h.connectStream("thread-a", 4, 1);
  assert.equal(h.streams[0].closed, true);
  h.stopStream();
  pending[2].resolve({ stream_ticket: "stopped" });
  await third;
  assert.equal(h.streams.length, 1);
  assert.equal(h.app.stream, null);
  const fourth = h.connectStream("thread-a", 4, 1);
  h.app.generation += 1;
  pending[3].reject(new Error("old selection failed"));
  await fourth;
  assert.equal(h.timers.size, 0);
  assert.deepEqual(h.statuses, []);
});

test("stream recovery handshake leaves retries to snapshot recovery until open", async () => {
  const h = streamHarness(async () => ({ stream_ticket: "recovery" }));
  const opened = h.connectStream("thread-a", 4, 1, true);
  await new Promise((resolve) => setImmediate(resolve));
  h.app.stream.onerror();
  await assert.rejects(opened, /did not reopen/);
  assert.equal(h.timers.size, 0);
  const replacement = h.connectStream("thread-a", 4, 1, true);
  await new Promise((resolve) => setImmediate(resolve));
  h.app.stream.onopen();
  await replacement;
  h.app.stream.onerror();
  assert.equal(h.timers.size, 1, "normal reconnect resumes after the handshake");
  h.stopStream();
});

test("records workspace restore-point receipts on their turn in order", () => {
  const state = createThreadState("thread-a");
  applySnapshot(state, snapshot("thread-a", 7));
  const pre = { kind: "pre_turn", snapshot_id: "c1", tree_id: "t1", session_id: "thread-a" };
  const post = { kind: "post_turn", snapshot_id: "c2", tree_id: "t2", session_id: "thread-a" };
  assert.equal(applyRuntimeEvent(state, runtimeEvent(8, "turn.workspace_snapshot", pre)), true);
  assert.equal(applyRuntimeEvent(state, runtimeEvent(9, "turn.workspace_snapshot", post)), true);
  assert.deepEqual(state.turns.get("turn-1").workspace_snapshots, [pre, post]);
  // A receipt for a turn the client has not seen still advances continuity.
  const orphan = runtimeEvent(10, "turn.workspace_snapshot", pre, { turn_id: "turn-9" });
  assert.equal(applyRuntimeEvent(state, orphan), true);
  assert.equal(state.latestSeq, 10);
  assert.equal(state.turns.has("turn-9"), false);
});
