import type { LocalizedText } from "./vocabulary";
import type { ModelFact, ProviderFact } from "../facts";

export const MODELS_COPY = {
  metaTitle: { en: "Models and providers · Codewhale", zh: "模型与提供商 · Codewhale" },
  metaDescription: { en: "Connect DeepSeek, Anthropic, OpenAI, Google, xAI or any OpenAI-compatible endpoint, then switch provider and model whenever the work changes.", zh: "连接 DeepSeek、Anthropic、OpenAI、Google、xAI 或任何兼容 OpenAI 的端点，工作变化时随时切换提供商和模型。" },
  title: { en: "Models and providers", zh: "模型与提供商" },
  lead: { en: "Codewhale works with the model providers you already pay for, and you can change the provider or model at any point in a session.", zh: "Codewhale 使用你已有的模型提供商，会话中随时可以更换提供商或模型。" },
  latestTitle: { en: "Recently added", zh: "最近加入" },
  latestLead: { en: "The latest addition to the catalog from each first-party provider. Dates are when Codewhale added the model, not the maker's release date.", zh: "每家一方提供商最近加入目录的模型。日期为 Codewhale 加入该模型的时间，不是厂商的发布日期。" },
  latestSince: { en: "In Codewhale since {date}", zh: "{date} 起已加入 Codewhale" },
  setupTitle: { en: "Connect a model in three steps", zh: "三步连接模型" },
  setupLead: { en: "The same steps work for every provider on this page.", zh: "本页的所有提供商都适用同样的步骤。" },
  steps: [
    {
      title: { en: "Save a key", zh: "保存密钥" },
      detail: { en: "Run `codewhale auth set --provider deepseek`, or the id of another provider from the table below, and paste the key.", zh: "运行 `codewhale auth set --provider deepseek`（或下表中其他提供商的 ID）并粘贴密钥。" },
    },
    {
      title: { en: "Pick a provider and model", zh: "选择提供商和模型" },
      detail: { en: "In a session, choose the provider with `/provider` and the model with `/model`.", zh: "在会话中，用 `/provider` 选择提供商，用 `/model` 选择模型。" },
    },
    {
      title: { en: "Switch any time", zh: "随时切换" },
      detail: { en: "Run `/provider` or `/model` again when the task changes. Your session stays open.", zh: "任务变化时再次运行 `/provider` 或 `/model`，会话保持不变。" },
    },
  ],
  routeDeepseekTitle: { en: "DeepSeek (default)", zh: "DeepSeek（默认）" },
  routeDeepseekBody: { en: "Save a DeepSeek key and start working. The default model is {model}.", zh: "保存 DeepSeek 密钥即可开始工作，默认模型为 {model}。" },
  routeFirstPartyTitle: { en: "Anthropic, OpenAI, Google and more", zh: "Anthropic、OpenAI、Google 等" },
  routeFirstPartyBody: { en: "Each provider reads its own credential setting.", zh: "每家提供商读取各自的凭据设置。" },
  routeCompatTitle: { en: "Any OpenAI-compatible endpoint", zh: "任何兼容 OpenAI 的端点" },
  routeCompatBody: { en: "Point Codewhale at any gateway or self-hosted server that speaks the OpenAI Chat Completions API, such as OrcaRouter or llama.cpp.", zh: "让 Codewhale 指向任何支持 OpenAI Chat Completions API 的网关或自托管服务，例如 OrcaRouter 或 llama.cpp。" },
  routeCompatConfig: { en: "Add a `[providers.<name>]` table to config.toml with `kind = \"openai-compatible\"` and its `base_url`, then select it with `provider = \"<name>\"`.", zh: "在 config.toml 中添加 `[providers.<name>]` 表，设置 `kind = \"openai-compatible\"` 与 `base_url`，再用 `provider = \"<name>\"` 选中它。" },
  providerDocs: { en: "Provider setup guides", zh: "提供商配置指南" },
  modelsTitle: { en: "Full model catalog", zh: "完整模型目录" },
  modelsLead: { en: "{count} models in the catalog of the published release, grouped by maker. A model reached through several providers is listed once.", zh: "已发布版本目录中的 {count} 个模型，按厂商分组。经多家提供商提供的同一模型只列一次。" },
  models: { en: "{count} models", zh: "{count} 个模型" },
  providerCount: { en: "Show all {count} provider routes", zh: "显示全部 {count} 条提供商路由" },
  colModel: { en: "Model", zh: "模型" },
  colContext: { en: "Context", zh: "上下文" },
  colAdded: { en: "In Codewhale since", zh: "加入 Codewhale 时间" },
  colKind: { en: "Type", zh: "类型" },
  reasoning: { en: "reasoning", zh: "推理" },
  listTitle: { en: "Providers and credential settings", zh: "提供商与凭据设置" },
  provider: { en: "Provider", zh: "提供商" },
  id: { en: "Provider ID", zh: "提供商 ID" },
  credential: { en: "Credential setting", zh: "凭据设置" },
  missing: { en: "Missing your provider?", zh: "没有找到你的提供商？" },
  request: { en: "Request support", zh: "申请支持" },
  otherMakers: { en: "Other makers", zh: "其他厂商" },
} satisfies Record<string, LocalizedText | { title: LocalizedText; detail: LocalizedText }[]>;

export const FIRST_PARTY_PROVIDER_IDS = [
  "deepseek", "anthropic", "openai", "openai-codex", "google", "xai",
  "mistral", "moonshot", "zai", "minimax", "meta", "xiaomi-mimo", "stepfun",
];

export const ROUTE_PROVIDERS = [
  { id: "anthropic", label: "Anthropic" },
  { id: "openai", label: "OpenAI" },
  { id: "google", label: "Google" },
  { id: "xai", label: "xAI" },
];

export const MODEL_MAKERS: { key: string; label: string; providers: string[] }[] = [
  { key: "anthropic", label: "Anthropic", providers: ["Anthropic"] },
  { key: "openai", label: "OpenAI", providers: ["OpenAI", "OpenAI Codex"] },
  { key: "google", label: "Google", providers: ["Google"] },
  { key: "deepseek", label: "DeepSeek", providers: ["DeepSeek"] },
  { key: "xai", label: "xAI", providers: ["xAI"] },
  { key: "moonshot", label: "Moonshot/Kimi", providers: ["Moonshot/Kimi"] },
  { key: "qwen", label: "Qwen", providers: ["Qwen"] },
  { key: "zai", label: "Z.ai", providers: ["Z.ai"] },
  { key: "minimax", label: "MiniMax", providers: ["MiniMax"] },
  { key: "mistral", label: "Mistral", providers: ["Mistral"] },
  { key: "meta", label: "Meta", providers: ["Meta"] },
  { key: "xiaomi", label: "Xiaomi", providers: ["Xiaomi MiMo"] },
  { key: "stepfun", label: "StepFun", providers: ["StepFun"] },
];

const FIRST_PARTY_MAKERS = ["anthropic", "openai", "google", "deepseek", "xai"];
const PREVIEW_ID = /(?:^|[-/])(exp|preview)(?:-|$)/;
const SMALL_TIER_ID = /(?:^|[-/])(mini|nano|lite|flash|haiku|small|fast)(?:-|$)/;

const byNewest = (a: ModelFact, b: ModelFact) =>
  (b.addedAt ?? "").localeCompare(a.addedAt ?? "") ||
  b.id.localeCompare(a.id, "en", { numeric: true });

const byNewestFlagship = (a: ModelFact, b: ModelFact) =>
  (b.addedAt ?? "").localeCompare(a.addedAt ?? "") ||
  Number(SMALL_TIER_ID.test(a.id)) - Number(SMALL_TIER_ID.test(b.id)) ||
  b.id.localeCompare(a.id, "en", { numeric: true });

export function makerKeyOf(model: ModelFact): string {
  const maker = MODEL_MAKERS.find((m) => model.provider !== null && m.providers.includes(model.provider));
  return maker?.key ?? "other";
}

export function groupModelsByMaker(models: ModelFact[]) {
  const groups = [...MODEL_MAKERS.map((m) => ({ key: m.key, label: m.label })), { key: "other", label: "" }]
    .map((g) => ({ ...g, models: models.filter((m) => makerKeyOf(m) === g.key).sort(byNewest) }));
  return groups.filter((g) => g.models.length > 0);
}

export function latestFirstPartyModels(models: ModelFact[]) {
  return FIRST_PARTY_MAKERS
    .map((key) => ({
      key,
      label: MODEL_MAKERS.find((m) => m.key === key)?.label ?? key,
      model: models
        .filter((m) => makerKeyOf(m) === key && m.addedAt !== null && !PREVIEW_ID.test(m.id))
        .sort(byNewestFlagship)[0],
    }))
    .flatMap((entry) => (entry.model ? [{ ...entry, model: entry.model }] : []));
}

export function splitCode(text: string): { code: boolean; text: string }[] {
  return text.split("`").map((part, i) => ({ code: i % 2 === 1, text: part })).filter((p) => p.text);
}

export function sortProvidersFirstPartyFirst(providers: ProviderFact[]): ProviderFact[] {
  const rank = (p: ProviderFact) => {
    const i = FIRST_PARTY_PROVIDER_IDS.indexOf(p.id);
    return i === -1 ? FIRST_PARTY_PROVIDER_IDS.length : i;
  };
  return [...providers].sort((a, b) => rank(a) - rank(b) || a.label.localeCompare(b.label));
}

export function formatModelTokens(n: number | null): string {
  if (n === null) return "—";
  if (n >= 1_000_000) {
    const m = n / 1_000_000;
    return `${Number.isInteger(m) ? m : m.toFixed(2)}M`;
  }
  if (n >= 1_000) return `${Math.round(n / 1_000)}K`;
  return String(n);
}

export function formatModelDate(iso: string | null, dateLocale: string): string {
  if (!iso) return "—";
  const d = new Date(`${iso}T00:00:00Z`);
  if (!Number.isFinite(d.getTime())) return "—";
  return new Intl.DateTimeFormat(dateLocale, {
    dateStyle: "medium",
    timeZone: "UTC",
  }).format(d);
}
