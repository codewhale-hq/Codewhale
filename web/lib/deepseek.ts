import { OUTBOUND_MAX_BYTES, readBoundedBody } from "./bounded-body";

const FALLBACK_BASE = "https://api.deepseek.com";
const FALLBACK_MODEL = "deepseek-flash";

export interface ChatMessage {
  role: "system" | "user" | "assistant";
  content: string;
}

interface ChatResponse {
  choices: { message: { content: string } }[];
  usage?: { prompt_tokens?: number; completion_tokens?: number };
}

export interface DeepSeekEnv {
  baseUrl?: string;
  model?: string;
}

export async function chat(
  messages: ChatMessage[],
  apiKey: string,
  jsonMode = false,
  dsEnv?: DeepSeekEnv
): Promise<string> {
  const data = await requestChat(messages, apiKey, jsonMode, dsEnv, { temperature: 0.4, maxTokens: 4096 });
  return data.choices[0]?.message?.content ?? "";
}

export async function requestChat(
  messages: ChatMessage[],
  apiKey: string,
  jsonMode: boolean,
  dsEnv: DeepSeekEnv | undefined,
  options: { temperature: number; maxTokens: number },
): Promise<ChatResponse> {
  const base = dsEnv?.baseUrl ?? process.env.DEEPSEEK_BASE_URL ?? FALLBACK_BASE;
  const model = dsEnv?.model ?? process.env.DEEPSEEK_MODEL ?? FALLBACK_MODEL;
  const res = await fetch(`${base}/v1/chat/completions`, {
    method: "POST",
    signal: AbortSignal.timeout(180_000),
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${apiKey}`,
    },
    body: JSON.stringify({
      model,
      messages,
      temperature: options.temperature,
      max_tokens: options.maxTokens,
      reasoning_effort: "high",
      ...(jsonMode ? { response_format: { type: "json_object" } } : {}),
    }),
  });
  let bytes: Uint8Array;
  try {
    bytes = await readBoundedBody(res, res.ok ? OUTBOUND_MAX_BYTES : 4096);
  } catch (cause) {
    throw new Error(`DeepSeek ${res.status}: ${cause instanceof Error ? cause.message : "body read failed"}`, { cause });
  }
  const text = new TextDecoder().decode(bytes);
  if (!res.ok) throw new Error(`DeepSeek ${res.status}: ${text}`);
  return JSON.parse(text) as ChatResponse;
}
