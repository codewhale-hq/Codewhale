const base = process.env.FAKE_CHAT_URL;

export const LoggerLevel = { info: 3 };
export const Domain = { Feishu: "https://feishu.invalid", Lark: "https://lark.invalid" };

async function record(chatId, data) {
  await fetch(`${base}/fake/reply`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ platform: "feishu", to: chatId, text: JSON.parse(data.content).text })
  });
}

const chatOfMessage = new Map();

export class Client {
  constructor() {
    this.im = {
      v1: {
        message: {
          create: async ({ params, data }) => record(data.receive_id || params?.receive_id, data),
          reply: async ({ path, data }) => record(chatOfMessage.get(path.message_id) || "unknown", data)
        }
      }
    };
  }
}

export class EventDispatcher {
  register(handlers) {
    this.handlers = handlers;
    return this;
  }
}

export class WSClient {
  start({ eventDispatcher }) {
    const poll = async () => {
      try {
        const events = await (await fetch(`${base}/fake/next?platform=feishu`)).json();
        for (const event of events) {
          chatOfMessage.set(event.message.message_id, event.message.chat_id);
          await eventDispatcher.handlers["im.message.receive_v1"](event);
        }
      } catch {}
      setTimeout(poll, 50);
    };
    poll();
  }
}
