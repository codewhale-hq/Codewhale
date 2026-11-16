const base = process.env.FAKE_CHAT_URL;

export function generateReqId(prefix = "req") {
  return `${prefix}-${Math.random().toString(36).slice(2)}`;
}

export class WSClient {
  constructor() {
    this.handlers = {};
  }
  on(name, handler) {
    this.handlers[name] = handler;
  }
  connect() {
    const poll = async () => {
      try {
        const frames = await (await fetch(`${base}/fake/next?platform=wecom`)).json();
        for (const frame of frames) await this.handlers.message?.(frame);
      } catch {}
      setTimeout(poll, 50);
    };
    poll();
  }
  async replyStream(frame, id, text) {
    await fetch(`${base}/fake/reply`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ platform: "wecom", to: frame.body.from.userid, text })
    });
  }
  async replyWelcome() {}
}
