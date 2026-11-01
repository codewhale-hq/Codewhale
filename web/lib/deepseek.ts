import { OUTBOUND_MAX_BYTES, readBoundedBody } from "./bounded-body";
import type { CuratedDispatch, FeedItem, RepoStats } from "./types";

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

const SYSTEM_PROMPT = `You are the editor of "今日要闻 / Today's Dispatch", a daily-ish digest for the Codewhale open source project.

You receive: repo stats and a list of recently updated issues, PRs, and releases.
Output a single JSON object — no prose around it — matching this exact shape:

{
  "headline": "string — English editorial headline (max ~70 chars)",
  "summary": "string — 2-3 English sentences, calm factual editorial voice",
  "highlights": [
    { "title": "string", "href": "string", "tag": "shipped|merged|opened|discussion|release", "blurb": "one sentence, max ~120 chars" }
  ],
  "movers": [
    { "number": 123, "title": "string", "href": "string", "reason": "one short clause" }
  ],
  "headlineZh": "string — Chinese (zh-CN) editorial headline, rewritten natively, not translated",
  "summaryZh": "string — 2-3 Chinese sentences, native zh-CN prose",
  "highlightsZh": [
    { "title": "string — zh-CN native", "href": "string", "tag": "shipped|merged|opened|discussion|release", "blurb": "zh-CN one sentence" }
  ],
  "moversZh": [
    { "number": 123, "title": "string — zh-CN", "href": "string", "reason": "zh-CN clause" }
  ]
}

Rules:
- Pick 3-5 highlights and 3-5 movers from the actual provided items. Never invent.
- Prefer items with discussion, merged PRs, recent releases, or labelled "good first issue".
- Tone: like a small-paper editor — measured, specific, never breathless.
- Never use words like "exciting", "amazing", "powerful", "revolutionary" (or Chinese equivalents like 令人兴奋, 强大无比, 革命性).
- href must be the html_url provided.
- The zh-CN fields must be native Chinese prose — not a direct translation of the English fields. Write them as a Chinese-speaking maintainer would.
- zh-CN uses full-width punctuation in CJK sentences (。，、).`;

export async function curate(
  apiKey: string,
  stats: RepoStats,
  feed: FeedItem[],
  dsEnv?: DeepSeekEnv
): Promise<CuratedDispatch> {
  const trimmedFeed = feed.slice(0, 25).map((f) => ({
    kind: f.kind,
    number: f.number,
    title: f.title,
    state: f.state,
    href: f.url,
    author: f.author,
    updated: f.updatedAt,
    comments: f.comments,
    labels: f.labels.map((l) => l.name),
  }));

  const userPayload = {
    repo: "codewhale-hq/CodeWhale",
    stats: {
      stars: stats.stars,
      forks: stats.forks,
      open_issues: stats.openIssues,
      open_pulls: stats.openPulls,
      latest_release: stats.latestRelease?.tag,
    },
    recent: trimmedFeed,
  };

  const raw = await chat(
    [
      { role: "system", content: SYSTEM_PROMPT },
      { role: "user", content: JSON.stringify(userPayload, null, 2) },
    ],
    apiKey,
    true,
    dsEnv
  );

  const parsed = JSON.parse(raw) as Omit<CuratedDispatch, "generatedAt">;
  return { ...sanitizeDispatch(parsed), generatedAt: new Date().toISOString() };
}

const SAFE_HREF_RE = /^https:\/\/(?:github\.com|api\.github\.com|codewhale\.net|crates\.io|www\.npmjs\.com|docs\.rs)\//;
const FALLBACK_HREF = "https://github.com/codewhale-hq/CodeWhale";

function safeHref(u: unknown): string {
  return typeof u === "string" && SAFE_HREF_RE.test(u) ? u : FALLBACK_HREF;
}

function sanitizeDispatch(d: Omit<CuratedDispatch, "generatedAt">): Omit<CuratedDispatch, "generatedAt"> {
  return {
    ...d,
    highlights: (d.highlights ?? []).map((h) => ({ ...h, href: safeHref(h.href) })),
    movers: (d.movers ?? []).map((m) => ({ ...m, href: safeHref(m.href) })),
    highlightsZh: (d.highlightsZh ?? []).map((h) => ({ ...h, href: safeHref(h.href) })),
    moversZh: (d.moversZh ?? []).map((m) => ({ ...m, href: safeHref(m.href) })),
  };
}
