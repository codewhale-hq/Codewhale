import type { ConstitutionDict } from "../types";

/** Chinese dictionary for `app/[locale]/constitution/page.tsx` and its `components/thinking-trace.tsx`. */
export const constitution: ConstitutionDict = {
  metaTitle: "三层规则 · Codewhale",
  metaDescription:
    "Codewhale 的嵌套宪章：内置基础规则、你的常备规则（/constitution）、仓库自己的规则（.codewhale/constitution.json）。位阶由执行框架强制生效，换掉模型也不失效。",
  kicker: "立论",
  title: "三层规则",
  titleAside: "Three layers of rules",
  titleAsideLang: "en",
  lede: "项目一变老，指令就开始堆积、彼此冲突：最初的规格、后来推翻它的重构、陈旧的记忆、上一个智能体的交接、你此刻的要求、刚跑出的与交接说法不符的测试结果。扁平的系统提示词让模型靠猜来化解；Codewhale 用一部嵌套的宪章给出明确的位阶。顺序由执行框架强制生效——有测试断言它不会漂移——换掉模型，结构依然完好。",
  since: "自 v0.9.0 起",
  sinceBody:
    "宪章优先的初始设置——首次启动依次引导语言、模型、权限和你的宪章；之后随时 /setup。模型可以起草，由你批准。",
  rankTitle: "位阶，从最稳到最活",
  rankScope:
    "三层之下依次是项目说明（AGENTS.md）、记忆与交接。你此刻的要求和实时工具证据仍然主宰当前回合——模型可以被给到很多层，但它被要求不去报告工具没有返回的事实。",
  boundaryTitle: "代码层面始终强制的部分",
  boundaryBody: "审批、沙箱、网络与信任控制由代码强制执行——宪章文本永远越不过它们。",
  traceTitle: "在推理里可以被看到",
  traceScope:
    "位阶会体现在模型的推理里：它在裁决时直接援引条款。下面是示意，概括了这类推理的样子，并非某一次会话的逐字记录。",
  illustration: "示意",
  install: "安装",
  configuration: "配置文档",
};
