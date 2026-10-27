import { ThreadStore, createRuntimeClient } from "../../bridge-core/src/lib.mjs";
import { DingTalkBridge, createReplySender, readConfig } from "./lib.mjs";
import { mkdir, open, unlink } from "node:fs/promises";
import path from "node:path";

async function main() {
  const config = readConfig();
  if (process.argv.includes("--validate-config")) {
    console.log("DingTalk bridge configuration is valid; no connection was opened.");
    return;
  }
  const sdkWarning = console.warn.bind(console);
  console.warn = () => sdkWarning("DingTalk SDK warning; connection state is retained.");
  const { DWClient, TOPIC_ROBOT } = await import("dingtalk-stream");
  const client = new DWClient({ clientId: config.clientId, clientSecret: config.clientSecret, debug: false, autoReconnect: true });
  await mkdir(path.dirname(config.statePath), { recursive: true, mode: 0o700 });
  const lockPath = `${config.statePath}.lock`;
  const lock = await open(lockPath, "wx", 0o600);
  let store;
  try {
    await lock.writeFile(String(process.pid));
    await lock.sync();
    store = await ThreadStore.open(config.statePath, { messageLimit: 2000, privateMode: true });
  } catch (error) {
    await lock.close();
    await unlink(lockPath);
    throw error;
  }
  const { runtimeJson, authHeaders } = createRuntimeClient(config);
  const bridge = new DingTalkBridge({
    config, store, runtimeJson, sendReply: createReplySender({ client }),
    runtimeFetch: (route, signal) => fetch(`${config.runtimeUrl}${route}`, { headers: { ...authHeaders(), accept: "text/event-stream" }, signal: AbortSignal.any([signal, AbortSignal.timeout(60000)]) }),
    report: (message) => console.error(message)
  });
  client.registerCallbackListener(TOPIC_ROBOT, async (frame) => {
    try {
      await bridge.accept(frame, () => client.socketCallBackResponse(frame.headers.messageId, { errcode: 0, errmsg: "ok" }));
    } catch {
      console.error("DingTalk message was not acknowledged; durable acceptance failed.");
    }
  });
  let closing = false;
  const shutdown = async () => {
    if (closing) return;
    closing = true;
    client.disconnect();
    await bridge.stop();
    await lock.close();
    await unlink(lockPath);
  };
  for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, () => { void shutdown().catch(() => { process.exitCode = 1; }); });
  try {
    await bridge.start();
    if (!closing) await client.connect();
    if (!closing) console.log("DingTalk Stream bridge started; SDK reconnect is enabled.");
  } catch (error) {
    await shutdown();
    throw error;
  }
}

main().catch(() => { console.error("DingTalk bridge startup failed. Check configuration, runtime availability and the state lock."); process.exitCode = 1; });
