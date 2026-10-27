import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

async function readBridgeSource() {
  return fs.readFile(path.join(__dirname, "../src/index.mjs"), "utf8");
}

function extractFunction(source, name) {
  const asyncMarker = `async function ${name}`;
  const marker = source.includes(asyncMarker) ? asyncMarker : `function ${name}`;
  const start = source.indexOf(marker);
  assert.notEqual(start, -1, `${name} should exist`);

  let depth = 0;
  let opened = false;
  const bodyStart = source.indexOf("{", source.indexOf(")", start));
  for (let index = bodyStart; index < source.length; index += 1) {
    const char = source[index];
    if (char === "{") {
      depth += 1;
      opened = true;
    } else if (char === "}") {
      depth -= 1;
      if (opened && depth === 0) {
        return source.slice(start, index + 1);
      }
    }
  }
  assert.fail(`${name} body should close`);
}

test("prompt command persists admission before starting background recovery", async () => {
  const source = await readBridgeSource();
  const handleCommand = extractFunction(source, "handleCommand");
  const promptCase = handleCommand.slice(handleCommand.indexOf('case "prompt":'));

  assert.match(source, /const activeTurnTasks = new Map\(\);/);
  assert.match(promptCase, /await startPromptTurn\(chatId, action\.prompt, identity\);/);
  assert.doesNotMatch(promptCase, /await\s+runPrompt\(/);

  const starter = extractFunction(source, "startPromptTurn");
  assert.ok(
    starter.indexOf("await threadStore.recordMessage") < starter.lastIndexOf("startTrackedTurnStream(chatId)"),
    "prompt and inbound receipt must be durable before admission starts"
  );
});

test("stale callback acknowledgements cannot skip modal actions", async () => {
  const source = await readBridgeSource();
  const callbackHandler = extractFunction(source, "handleCallbackQuery");

  assert.doesNotMatch(callbackHandler, /await\s+answerCallback\(query\.id,\s*"Working\.\.\."\)/);
  assert.match(callbackHandler, /answerCallback\(query\.id,\s*"Working\.\.\."\)\.catch/);
  assert.match(callbackHandler, /await handleModalAction\(identity\.chatId, action, query, identity\);/);
});

test("polling persists offsets only after successful update handling", async () => {
  const source = await readBridgeSource();
  const startup = source.slice(
    source.indexOf("const threadStore = await ThreadStore.open"),
    source.indexOf("function requestStop")
  );
  const pollTelegram = extractFunction(source, "pollTelegram");
  const markUpdateHandled = extractFunction(source, "markUpdateHandled");

  assert.match(
    startup,
    /let updateOffset = threadStore\.getCursor\(\s*"telegram\.update_offset",\s*Number\(process\.env\.TELEGRAM_UPDATE_OFFSET \|\| 0\)\s*\);/
  );
  assert.doesNotMatch(pollTelegram, /updateOffset = Math\.max\(updateOffset, update\.update_id \+ 1\)/);
  assert.match(pollTelegram, /await handleIncomingUpdate\(update\);\s*await markUpdateHandled\(update\);/);
  assert.match(
    pollTelegram,
    /catch \(error\) {\s*console\.error\("failed to handle incoming Telegram update", error\);\s*break;\s*}/
  );
  assert.match(markUpdateHandled, /const nextOffset = Math\.max\(updateOffset, Number\(update\.update_id\) \+ 1\);/);
  assert.match(markUpdateHandled, /await threadStore\.setCursor\("telegram\.update_offset", updateOffset\);/);
});

test("polling conflicts use bounded escalation instead of a flat retry loop", async () => {
  const source = await readBridgeSource();
  const pollTelegram = extractFunction(source, "pollTelegram");

  assert.match(source, /telegramPollingConflictDelayMs/);
  assert.match(pollTelegram, /let pollingConflictAttempts = 0;/);
  assert.match(pollTelegram, /telegramPollingConflictDelayMs\(pollingConflictAttempts\)/);
  assert.match(pollTelegram, /pollingConflictAttempts \+= 1;/);
  assert.match(pollTelegram, /throw new Error\(/);
  assert.doesNotMatch(pollTelegram, /Retrying in 10s/);
  assert.doesNotMatch(pollTelegram, /await delay\(10000\)/);
});

test("callback replay is ignored before modal dispatch", async () => {
  const source = await readBridgeSource();
  const incomingHandler = extractFunction(source, "handleIncomingUpdate");
  const replayHelper = extractFunction(source, "isReplayCallbackUpdate");
  const storedAction = extractFunction(source, "handleStoredAction");
  const resumeCase = storedAction.slice(storedAction.indexOf('if (stored.kind === "resume")'));

  assert.match(incomingHandler, /if \(await isReplayCallbackUpdate\(update\)\) return;\s*await handleCallbackQuery\(update\.callback_query\);/);
  assert.match(replayHelper, /if \(update\.update_id == null\) return false;/);
  assert.match(replayHelper, /return threadStore\.recordMessage\(`callback:\$\{update\.update_id\}`\);/);
  assert.ok(
    resumeCase.indexOf("await threadStore.takeAction(action.token, owner);") <
      resumeCase.indexOf("await resumeThread(chatId, stored.threadId, identity);"),
    "resume callback actions should be consumed before dispatch"
  );
});

test("reattached streams are detached and shutdown preserves active turn state", async () => {
  const source = await readBridgeSource();
  const reattach = extractFunction(source, "reattachActiveTurns");
  const runPrompt = extractFunction(source, "runPrompt");

  assert.match(reattach, /startTrackedTurnStream\(chatId\);/);
  assert.doesNotMatch(reattach, /await\s+streamTurnEvents\(/);
  assert.match(source, /async function clearActiveTurn\(chatId\)/);
  assert.match(runPrompt, /if \(state\.turnTerminal \|\| await streamTurnEvents/);
  assert.match(runPrompt, /await clearActiveTurn\(chatId\);/);

  const trackedStream = extractFunction(source, "startTrackedTurnStream");
  assert.doesNotMatch(trackedStream, /clearActiveTurn/);
  assert.match(trackedStream, /while \(!stopping && !controller\.signal\.aborted\)/);
});

test("turn update sends retry without ending the stream", async () => {
  const source = await readBridgeSource();
  const streamTurnEvents = extractFunction(source, "streamTurnEvents");
  const sendTurnText = extractFunction(source, "sendTurnText");
  const telegramApi = extractFunction(source, "telegramApi");

  assert.doesNotMatch(streamTurnEvents, /await\s+sendText\(/);
  assert.match(streamTurnEvents, /await\s+sendTurnText\(/);
  assert.match(sendTurnText, /catch \(error\) {\s*console\.error\("failed to send Telegram turn update"/);
  assert.match(telegramApi, /method === "sendMessage" \? telegramSendRetryDelayMs\(error, attempt\) : null/);
});

test("turn streams keep Telegram typing visible and pause while waiting for approval", async () => {
  const source = await readBridgeSource();
  const streamTurnEvents = extractFunction(source, "streamTurnEvents");
  const sendTypingAction = extractFunction(source, "sendTypingAction");
  const telegramApiOnce = extractFunction(source, "telegramApiOnce");

  assert.match(source, /const TYPING_INTERVAL_MS = 2000;/);
  assert.match(source, /const TYPING_TIMEOUT_MS = 1500;/);
  assert.match(streamTurnEvents, /let typingPaused = false;/);
  assert.match(streamTurnEvents, /let typingInFlight = false;/);
  assert.match(streamTurnEvents, /const typingTimer = setInterval\(\(\) => {\s*void tickTyping\(\);/);
  assert.match(streamTurnEvents, /void tickTyping\(\);/);
  assert.match(streamTurnEvents, /const stopTypingEvent =/);
  assert.match(streamTurnEvents, /if \(typingPaused && !\["approval\.required", "user_input\.required"\]\.includes\(record\.event\) && !stopTypingEvent\)/);
  assert.match(streamTurnEvents, /typingPaused = true;/);
  assert.match(streamTurnEvents, /clearInterval\(typingTimer\);/);
  assert.match(sendTypingAction, /telegramApi\(\s*"sendChatAction"/);
  assert.match(sendTypingAction, /action: "typing"/);
  assert.match(sendTypingAction, /setTimeout\(\(\) => controller\.abort\(\), TYPING_TIMEOUT_MS\)/);
  assert.match(telegramApiOnce, /signal: options\.signal/);
});

test("turn streams debounce last-seq writes and flush before exit", async () => {
  const source = await readBridgeSource();
  const streamTurnEvents = extractFunction(source, "streamTurnEvents");
  const flushLastSeq = extractFunction(source, "flushLastSeq");
  const streamWithoutFlushHelper = streamTurnEvents.replace(flushLastSeq, "");

  assert.match(source, /const LAST_SEQ_FLUSH_INTERVAL_MS = 2000;/);
  assert.doesNotMatch(
    streamWithoutFlushHelper,
    /await threadStore\.patchChat\(chatId, \{ lastSeq: latestSeq, responseText, turnTerminal \}\);/
  );
  assert.match(streamTurnEvents, /await flushLastSeq\(false\);/);
  assert.match(streamTurnEvents, /await flushLastSeq\(true\);/);
  assert.match(flushLastSeq, /if \(latestSeq <= flushedSeq\) return;/);
  assert.match(flushLastSeq, /Date\.now\(\) - lastSeqFlushAt < LAST_SEQ_FLUSH_INTERVAL_MS/);
  assert.match(flushLastSeq, /await threadStore\.patchChat\(chatId, \{ lastSeq: latestSeq, responseText, turnTerminal \}\);/);
  assert.match(flushLastSeq, /flushedSeq = latestSeq;/);
});

test("Telegram sends MarkdownV2 with plain-text fallback on parse errors", async () => {
  const source = await readBridgeSource();
  const sendText = extractFunction(source, "sendText");

  assert.match(sendText, /telegramMessageBody\(chunk, \{ markdown: true, maxChars: config\.maxReplyChars \}\)/);
  assert.match(sendText, /isTelegramMarkdownParseError\(error\)/);
  assert.match(sendText, /telegramMessageBody\(chunk, \{ markdown: false, maxChars: config\.maxReplyChars \}\)/);
  assert.match(sendText, /await telegramApi\("sendMessage", fallbackBody\);/);
});
