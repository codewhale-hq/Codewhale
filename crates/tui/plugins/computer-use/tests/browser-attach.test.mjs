// Attach mode: the plugin drives the computer's one shared Chromium through a
// NUL-framed CDP Unix socket (the cw-cdp-bridge of codewhale-computing). It
// never launches, never closes a tab, never closes the browser.
import { test, after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { createBrowser, connectPipeSocket, attachSocket } from "../src/browser-cdp.mjs";
// These transports are Unix sockets inside the Linux computer; Windows cannot bind the path.
const UNIX_SOCKETS = { skip: process.platform === "win32" && "Unix-socket transport (Linux only)" };

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "cu-attach-"));
after(() => fs.rmSync(dir, { recursive: true, force: true }));

/** A fake bridge: one client at a time, NUL framing, a tiny CDP browser. */
function fakeBridge(sock, { tabs = [] } = {}) {
  const calls = [];
  const targets = [...tabs];
  let client = null;
  let n = 0;
  const server = net.createServer((s) => {
    if (client && !client.destroyed) { s.end('{"error":"cdp_busy"}\0'); return; }
    client = s;
    let buf = Buffer.alloc(0);
    const send = (obj) => s.write(`${JSON.stringify(obj)}\0`);
    s.on("data", (chunk) => {
      buf = Buffer.concat([buf, chunk]);
      let i;
      while ((i = buf.indexOf(0)) >= 0) {
        const msg = JSON.parse(buf.subarray(0, i).toString());
        buf = buf.subarray(i + 1);
        calls.push(msg.method);
        const reply = (result) => send({ id: msg.id, result, ...(msg.sessionId ? { sessionId: msg.sessionId } : {}) });
        switch (msg.method) {
          case "Browser.getVersion": reply({ product: "Chrome/150.0.7871.100" }); break;
          case "Target.getTargets": reply({ targetInfos: targets.map((t) => ({ ...t, type: "page" })) }); break;
          case "Target.createTarget": { const t = { targetId: `agent-${++n}`, url: "about:blank", title: "" }; targets.push(t); reply({ targetId: t.targetId }); break; }
          case "Target.attachToTarget": reply({ sessionId: `s-${msg.params.targetId}` }); break;
          case "Target.getTargetInfo": { const t = targets.find((x) => x.targetId === msg.params.targetId); reply({ targetInfo: { ...t, type: "page" } }); break; }
          case "Page.navigate": {
            const t = targets.find((x) => `s-${x.targetId}` === msg.sessionId);
            t.url = msg.params.url; t.title = "Example";
            reply({ frameId: "f" });
            setTimeout(() => send({ method: "Page.loadEventFired", params: {}, sessionId: msg.sessionId }), 5);
            break;
          }
          default: reply({});
        }
      }
    });
    s.on("close", () => { if (client === s) client = null; });
  });
  return new Promise((resolve) => server.listen(sock, () => resolve({ server, calls, targets })));
}

test("attachSocket reads CODEWHALE_CU_BROWSER_ATTACH", () => {
  assert.equal(attachSocket({}), null);
  assert.equal(attachSocket({ CODEWHALE_CU_BROWSER_ATTACH: " /run/cw/cdp.sock " }), "/run/cw/cdp.sock");
});

test("attach: opens a visible tab beside the person's, lists their tabs, stop only detaches", UNIX_SOCKETS, async (t) => {
  const sock = path.join(dir, "a.sock");
  const bridge = await fakeBridge(sock, { tabs: [{ targetId: "human-1", url: "https://news.example/", title: "News" }] });
  t.after(() => bridge.server.close());
  const launched = [];
  const browser = createBrowser({ attach: sock, launch: (x) => launched.push(x), findApp: () => { throw new Error("must not look for an app"); }, recordingsDir: () => dir });
  const started = await browser.start({ url: "https://example.com/" });
  assert.equal(started.attached, true);
  assert.equal(started.shared, true);
  assert.equal(started.browser, "Chrome/150.0.7871.100");
  assert.equal(started.tab.url, "https://example.com/");
  assert.equal(started.verified, true);
  assert.equal(launched.length, 0, "attach mode launches nothing");
  assert.ok(bridge.calls.includes("Target.createTarget"), "a person's tab is never taken implicitly");
  assert.ok(bridge.calls.includes("Target.activateTarget"), "the agent's tab is brought to the front");

  // A tab the person opens shows up in the agent's view.
  bridge.targets.push({ targetId: "human-2", url: "https://mail.example/", title: "Mail" });
  const status = await browser.status();
  assert.deepEqual(status.tabs.map((x) => x.id).sort(), ["agent-1", "human-1", "human-2"]);
  assert.equal(status.tabs.find((x) => x.id === "agent-1").agent, true);

  // Moving to a named tab of the person's is explicit.
  const moved = await browser.start({ tab: "human-2" });
  assert.equal(moved.switched_tab, true);
  assert.equal(moved.activeTab.id, "human-2");

  const stopped = await browser.stop();
  assert.equal(stopped.detached, true);
  assert.equal(stopped.browser_closed, false);
  assert.ok(!bridge.calls.includes("Target.closeTarget"), "no tab is closed");
  assert.ok(!bridge.calls.includes("Browser.close"), "the shared browser is never closed");
  assert.ok(bridge.calls.includes("Target.detachFromTarget"));
});

test("attach: adopts a lone blank tab instead of stacking a second", UNIX_SOCKETS, async (t) => {
  const sock = path.join(dir, "b.sock");
  const bridge = await fakeBridge(sock, { tabs: [{ targetId: "blank", url: "chrome://newtab/", title: "New Tab" }] });
  t.after(() => bridge.server.close());
  const browser = createBrowser({ attach: sock, recordingsDir: () => dir });
  const started = await browser.start({});
  assert.equal(started.tab.id, "blank");
  assert.ok(!bridge.calls.includes("Target.createTarget"));
  await browser.close();
});

test("attach: a second controller gets browser_busy; a missing bridge gets browser_unavailable", UNIX_SOCKETS, async (t) => {
  const sock = path.join(dir, "c.sock");
  const bridge = await fakeBridge(sock, { tabs: [] });
  t.after(() => bridge.server.close());
  const holder = await connectPipeSocket(sock);
  t.after(() => holder.close());
  await new Promise((r) => setTimeout(r, 20));
  const browser = createBrowser({ attach: sock, recordingsDir: () => dir });
  await assert.rejects(browser.start({}), (e) => e.code === "browser_busy");
  const missing = createBrowser({ attach: path.join(dir, "nope.sock"), recordingsDir: () => dir });
  await assert.rejects(missing.start({}), (e) => e.code === "browser_unavailable");
  await assert.rejects(createBrowser({ recordingsDir: () => dir, attach: null }).start({ tab: "x" }), (e) => e.code === "bad_args");
});
