import type { LocalizedText } from "./vocabulary";
import type { ModelFact, ProviderFact } from "../facts";

export const MODELS_COPY = {
  metaTitle: { en: "Models and providers · Codewhale", zh: "模型与提供商 · Codewhale" },
  metaDescription: { en: "Connect DeepSeek, Anthropic, OpenAI, Google, xAI or any OpenAI-compatible endpoint, then switch provider and model whenever the work changes.", zh: "连接 DeepSeek、Anthropic、OpenAI、Google、xAI 或任何兼容 OpenAI 的端点，工作变化时随时切换提供商和模型。" },
  title: { en: "Models and providers", zh: "模型与提供商" },
  lead: { en: "Codewhale works with the model providers you already pay for, and you can change the provider or model at any point in a session.", zh: "Codewhale 使用你已有的模型提供商，会话中随时可以更换提供商或模型。" },
  latestTitle: { en: "Latest models", zh: "最新模型" },
  latestLead: { en: "The newest model from each first-party provider, by when it entered Codewhale.", zh: "每家一方提供商最新的模型，按进入 Codewhale 的时间确定。" },
  latestSince: { en: "In Codewhale since {date}", zh: "{date} 起已加入 Codewhale" },
  setupTitle: { en: "Connect a model in three steps", zh: "三步连接模型" },
  setupLead: { en: "The same steps work for every provider on this page.", zh: "本页的所有提供商都适用同样的步骤。" },
  steps: [
    {
      title: { en: "Save a key", zh: "保存密钥" },
      detail: { en: "Run codewhale auth set --provider <id> and paste the key.", zh: "运行 codewhale auth set --provider <id> 并粘贴密钥。" },
    },
    {
      title: { en: "Pick a provider and model", zh: "选择提供商和模型" },
      detail: { en: "In a session, choose the provider with /provider and the model with /model.", zh: "在会话中，用 /provider 选择提供商，用 /model 选择模型。" },
    },
    {
      title: { en: "Switch any time", zh: "随时切换" },
      detail: { en: "Run /provider or /model again when the task changes. Your session stays open.", zh: "任务变化时再次运行 /provider 或 /model，会话保持不变。" },
    },
  ],
  routeDeepseekTitle: { en: "DeepSeek (default)", zh: "DeepSeek（默认）" },
  routeDeepseekBody: { en: "Save a DeepSeek key and start working. The default model is {model}.", zh: "保存 DeepSeek 密钥即可开始工作，默认模型为 {model}。" },
  routeFirstPartyTitle: { en: "Anthropic, OpenAI, Google and more", zh: "Anthropic、OpenAI、Google 等" },
  routeFirstPartyBody: { en: "Each provider reads its own credential setting.", zh: "每家提供商读取各自的凭据设置。" },
  routeCompatTitle: { en: "Any OpenAI-compatible endpoint", zh: "任何兼容 OpenAI 的端点" },
  routeCompatBody: { en: "Point Codewhale at a gateway such as OrcaRouter or OpenRouter, or at a server you host such as vLLM, SGLang, llama.cpp or Ollama.", zh: "让 Codewhale 指向 OrcaRouter、OpenRouter 等网关，或 vLLM、SGLang、llama.cpp、Ollama 等自托管服务。" },
  routeCompatConfig: { en: "Add it as a custom provider with kind = \"openai-compatible\" and its base_url.", zh: "将其添加为自定义提供商，设置 kind = \"openai-compatible\" 与 base_url。" },
  providerDocs: { en: "Provider setup guides", zh: "提供商配置指南" },
  modelsTitle: { en: "Full model catalog", zh: "完整模型目录" },
  modelsLead: { en: "{count} models declared in Codewhale v{version}, grouped by maker. A model reached through several providers is listed once.", zh: "Codewhale v{version} 声明的 {count} 个模型，按厂商分组。经多家提供商提供的同一模型只列一次。" },
  models: { en: "{count} models", zh: "{count} 个模型" },
  providerCount: { en: "{count} providers", zh: "{count} 个提供商" },
  colModel: { en: "Model", zh: "模型" },
  colContext: { en: "Context", zh: "上下文" },
  colAdded: { en: "In Codewhale since", zh: "加入 Codewhale 时间" },
  reasoning: { en: "reasoning", zh: "推理" },
  listTitle: { en: "All providers and credential settings", zh: "全部提供商与凭据设置" },
  listLead: { en: "Providers declared in Codewhale v{version}, first-party providers first. Run /provider to see the choices in your installed version.", zh: "Codewhale v{version} 声明的提供商，一方提供商在前。运行 /provider 查看已安装版本中的可选项。" },
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

const byNewest = (a: ModelFact, b: ModelFact) =>
  (b.addedAt ?? "").localeCompare(a.addedAt ?? "") ||
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
        .sort(byNewest)[0],
    }))
    .flatMap((entry) => (entry.model ? [{ ...entry, model: entry.model }] : []))
    .sort((a, b) => byNewest(a.model, b.model));
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
