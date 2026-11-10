import type { LocalizedText } from "./vocabulary";

export const MODELS_COPY = {
  metaTitle: { en: "DeepSeek, Ollama and local models · Codewhale", zh: "DeepSeek、Ollama 与本地模型 · Codewhale" },
  metaDescription: { en: "Use Codewhale with DeepSeek, Ollama, other hosted providers or local models. Configure credentials and endpoints, then choose a model per session.", zh: "让 Codewhale 使用 DeepSeek、Ollama、其他托管提供商或本地模型。配置凭据与端点，再按会话选择模型。" },
  kicker: { en: "Models and providers", zh: "模型与提供商" },
  title: { en: "Pick the right model for each task.", zh: "为每项任务选择合适的模型。" },
  lead: { en: "Use a hosted model, a local server, or a gateway. Switch the provider or model whenever the work changes.", zh: "使用托管模型、本地服务或网关。工作变化时，随时切换提供商或模型。" },
  providerDocs: { en: "Provider setup guides", zh: "提供商配置指南" },
  install: { en: "Install Codewhale", zh: "安装 Codewhale" },
  setupLabel: { en: "Get connected", zh: "建立连接" },
  setupTitle: { en: "Set up your model connection", zh: "配置模型连接" },
  setupLead: { en: "Save your key with codewhale auth set --provider <id>. In a session, pick the provider with /provider and the model with /model; local servers use the endpoint and authentication you configure.", zh: "用 codewhale auth set --provider <id> 保存密钥。在会话中，用 /provider 选择提供商，用 /model 选择模型；本地服务使用你配置的端点与认证方式。" },
  patterns: [
    {
      title: { en: "DeepSeek", zh: "DeepSeek" },
      detail: { en: "Use a DeepSeek API key. The default model is {model}.", zh: "使用 DeepSeek API 密钥连接，默认模型为 {model}。" },
      reference: "DEEPSEEK_API_KEY",
    },
    {
      title: { en: "Local models", zh: "本地模型" },
      detail: { en: "Run models with Ollama, vLLM, or SGLang, then point Codewhale at the local endpoint.", zh: "用 Ollama、vLLM 或 SGLang 运行模型，再让 Codewhale 指向本地端点。" },
      reference: "ollama · vllm · sglang",
    },
    {
      title: { en: "OpenRouter", zh: "OpenRouter" },
      detail: { en: "Reach models from several providers through one gateway. Choose OpenRouter, then pick a model.", zh: "通过一个网关使用多家提供商的模型。先选择 OpenRouter，再选择模型。" },
      reference: "OPENROUTER_API_KEY",
    },
  ],
  listTitle: { en: "Supported providers", zh: "支持的提供商" },
  listLead: { en: "Providers declared in source v{version}. Run /provider to see the choices in your installed version; the setup guides cover credentials, endpoints, and model selection.", zh: "源码 v{version} 中声明的提供商。运行 /provider 查看已安装版本中的可选项；配置指南介绍凭据、端点与模型选择。" },
  modelsTitle: { en: "Models in source v{version}", zh: "源码 v{version} 中的模型" },
  modelsLead: { en: "Every model id the source declares for v{version}, merged across provider spellings. “Added” is the date the model first entered the source, not the provider’s release date. Sort any column; the default order is most recently added.", zh: "源码 v{version} 声明的全部模型 ID，已合并不同提供商拼写。“加入时间”是模型首次进入源码的日期，而非提供商自己的发布日期。各列均可排序，默认按最近加入排列。" },
  colModel: { en: "Model", zh: "模型" },
  colProvider: { en: "Provider", zh: "提供商" },
  colContext: { en: "Context", zh: "上下文" },
  colAdded: { en: "Added", zh: "加入时间" },
  reasoning: { en: "reasoning", zh: "推理" },
  sortAsc: { en: "Sort ascending", zh: "升序排列" },
  sortDesc: { en: "Sort descending", zh: "降序排列" },
  provider: { en: "Provider", zh: "提供商" },
  id: { en: "Provider ID", zh: "提供商 ID" },
  credential: { en: "Credential setting", zh: "凭据设置" },
  missing: { en: "Missing your provider?", zh: "没有找到你的提供商？" },
  request: { en: "Request support", zh: "申请支持" },
} satisfies Record<string, LocalizedText | { title: LocalizedText; detail: LocalizedText; reference: string }[]>;

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
