import type { EnterpriseDict } from "../en/enterprise";

export const enterprise: EnterpriseDict = {
  metaTitle: "Codewhale Enterprise · 开发中",
  metaDescription: "Codewhale Enterprise 把团队的工作变成评测、训练数据和你自己拥有的模型。开发中，欢迎设计合作伙伴。",
  kicker: "Codewhale Enterprise",
  status: "开发中",
  title: "拥有团队赖以工作的智能",
  lead: "团队的工作已经在 Codewhale 中进行。Enterprise 把这些工作变成评测、训练数据和你自己拥有的模型。目前处于开发阶段，欢迎设计合作伙伴加入。",
  partnerCta: "成为设计合作伙伴",
  secondaryCta: "试用开源智能体",

  loopTitle: "你将得到什么",
  loopScope: "每个模型在替换任何东西之前，都先在你自己的保留任务上测试。",
  loop: [
    {
      title: "定义“完成”的评测",
      body: "基于团队真实工作构建的评测套件，由保留集判断模型是否够好。",
    },
    {
      title: "来自你工作的训练数据",
      body: "数据取自团队已经认可的工作，并去除敏感信息。",
    },
    {
      title: "你自己拥有的模型",
      body: "由你掌控的微调开放权重模型，并在质量、成本和速度上与当前模型对比。",
    },
    {
      title: "持续改进模型的环境",
      body: "基于你自己的任务构建的强化学习环境，让模型在你最在意的工作上持续变好。",
    },
  ],

  statusTitle: "现已可用与计划中",
  statusScope: "开源智能体现已可用。Enterprise 正在开发中，目前还没有训练过客户模型。",
  todayTitle: "现已可用",
  today: [
    {
      title: "支持任何模型的智能体",
      body: "托管服务商、任何 OpenAI 兼容端点，以及你用 vLLM、SGLang 或 Ollama 自行部署的模型。",
    },
    {
      title: "带回执的已保存会话",
      body: "每个会话都保留工具调用、结果和审批记录，团队的工作有据可查。",
    },
  ],
  plannedTitle: "Enterprise 计划",
  planned: [
    {
      title: "托管的评测与训练环境",
      body: "运行评测和训练的环境，不再依赖笔记本电脑。",
    },
    {
      title: "每个新基座模型都重新验证",
      body: "更好的开放模型发布时，先在你的保留集上测试，通过后才会上线。",
    },
    {
      title: "在租用模型与自有模型之间路由",
      body: "每个工作流都交给每单位成本下被接受结果最好的模型，无论租用还是自有。",
    },
    {
      title: "私有部署",
      body: "在数据必须停留的地方运行智能体和 Enterprise。",
    },
  ],

  controlTitle: "由你掌控",
  dataTitle: "数据留在你选择的地方",
  dataBody: "你的会话、数据集和模型都留在你选择的地方。训练只在你批准的算力上进行。",
  engineTitle: "同一个带审批的 Rust 引擎",
  engineBody: "运行工作流和 Enterprise 的是同一个引擎。Ask、Auto-Review 和 Full Access 权限以及回执对两者同样适用。",

  partnerTitle: "设计合作伙伴",
  partnerBody: "带来一个团队反复做的工作流。你会得到当前模型在你自己保留集上的基线，再测试一个自有模型与之对比。发邮件即可开始。",
  partnerEmail: "help@codewhale.net",
};
