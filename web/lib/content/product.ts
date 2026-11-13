/**
 * product.ts — the copy for /product, the "what is this and what do I get"
 * page behind the primary nav's first link.
 *
 * TRUTH CONTRACT: availability is stated per surface as it is today. The
 * terminal, browser, hosted web, and desktop rows mirror the homepage's
 * availability chapter and docs/public-surface-facts.json; the GUI row states
 * the community graphical frontend — a separate project from this repository's
 * own VS Code extension — exactly as README.md links it.
 * Nothing here claims cloud execution; the hosted web app is not open;
 * desktop is a development build.
 * Counts (providers, tools, sandbox backends) come from the facts layer at
 * render time, never typed here.
 */

import { AVAILABILITY_ROWS } from "./availability";
import type { CapabilityClaim } from "../capabilities";
import { FACTS } from "../facts.generated";
import type { LocalizedText } from "./vocabulary";

const PUBLISHED_TAG = FACTS.latestPublishedRelease?.tag ?? "main";

export interface ProductRow {
  title: LocalizedText;
  body: LocalizedText;
  capability?: CapabilityClaim;
}

export type { ProductAvailabilityRow } from "./availability";

export const PRODUCT_COPY = {
  metadata: {
    title: { en: "Build and automate · Codewhale", zh: "构建与自动化 · Codewhale" },
    description: {
      en: "Build apps, automate workflows and work across connected tools with an open-source agent. Bring your existing model APIs or local and self-hosted inference.",
      zh: "用开源智能体构建应用、自动处理工作，并连接你常用的工具。使用你已有的模型 API、本地模型或自托管推理服务。",
    },
  },
  title: {
    en: "Build apps and automate workflows",
    zh: "构建应用，自动化工作流",
  },
  lede: {
    en: "Codewhale works in your terminal and local browser, using the models you choose.",
    zh: "Codewhale 在终端和本地浏览器中工作，使用你选择的模型。",
  },

  workHeading: { en: "Keep larger tasks moving", zh: "推进更大的任务" },
  workPatterns: [
    {
      title: { en: "Work", zh: "Work" },
      body: {
        en: "Plan, delegate and verify a larger task in Work.",
        zh: "在 Work 中计划、委派并验证较大的任务。",
      },
      href: "/docs/modes",
      command: "/mode work",
      capability: { feature: "modes" },
    },
    {
      title: { en: "Fleet", zh: "Fleet" },
      body: {
        en: "Give agents different roles and models, then reuse those profiles across tasks.",
        zh: "为智能体分配不同角色和模型，再在后续任务中复用这些配置。",
      },
      href: "/docs/fleet",
      command: "/fleet setup",
      capability: { feature: "fleet" },
    },
    {
      title: { en: "Workflows", zh: "Workflows" },
      body: {
        en: "Save steps, parallel work and review gates as a process you can run again.",
        zh: "把步骤、并行工作与审阅关卡保存为可重复运行的流程。",
      },
      href: `https://github.com/codewhale-hq/Codewhale/blob/${PUBLISHED_TAG}/docs/AUTOMATIC_WORKFLOWS.md`,
      command: "codewhale workflow run <workflow>",
      capability: { feature: "workflows" },
    },
    {
      title: { en: "Skills and plugins", zh: "技能与插件" },
      body: {
        en: "Save a checklist or report format in a Markdown skill, then share it inside a plugin.",
        zh: "用 Markdown 技能保存清单或报告格式，再放进插件中分享。",
      },
      href: "/docs/skills",
      command: "SKILL.md",
      capability: { feature: "skills" },
    },
    {
      title: { en: "Receipts and undo", zh: "回执与撤销" },
      body: {
        en: "Preview the next primary request, review recorded actions and restore available file snapshots.",
        zh: "预览下一次主要请求、查看已记录的操作，并恢复可用的文件快照。",
      },
      href: "/docs/review",
      command: "/receipts",
      capability: { feature: "receipts" },
    },
  ],

  availabilityHeading: { en: "Where it runs", zh: "运行平台" },
  availability: AVAILABILITY_ROWS.filter((row) => row.pages.includes("product")),

  controlHeading: { en: "Set permissions in the 0.10.3 source candidate", zh: "设置 0.10.3 源码候选版本的权限" },
  modes: [
    { title: { en: "Plan", zh: "Plan" }, capability: { feature: "modes" }, body: { en: "Blocks file mutation and shell execution. Permitted research may contact external services; session state can still be saved.", zh: "禁止文件修改与 shell 执行。获准的研究可访问外部服务；会话状态仍可保存。" } },
    { title: { en: "Work", zh: "Work" }, capability: { feature: "modes" }, body: { en: "Edits files and runs commands within the permission you set.", zh: "在你设定的权限内修改文件、运行命令。" } },
  ] satisfies ProductRow[],
  permissions: [
    { title: { en: "Ask", zh: "Ask" }, capability: { feature: "permission-postures" }, body: { en: "Prompts according to the active approval rules; saved permissions and hard policy boundaries still apply.", zh: "按当前审批规则询问；已保存的权限与强制策略边界仍然生效。" } },
    { title: { en: "Accept Edits", zh: "Accept Edits" }, capability: { feature: "permission-postures", describesCurrentRow: true }, body: { en: "Allows ordinary edits inside a git workspace and asks before commands or sensitive-file edits. New source-candidate installs use this posture; an explicit saved choice stays in control.", zh: "允许 Git 工作区内的普通文件编辑，并在运行命令或修改敏感文件前询问。新安装的源码候选版本使用此设置；明确保存的选择始终优先。" } },
    { title: { en: "Auto-Review", zh: "Auto-Review" }, capability: { feature: "permission-postures" }, body: { en: "Automatically reviews eligible actions and reports any action it cannot approve.", zh: "自动审核符合条件的操作，并报告无法批准的操作。" } },
    { title: { en: "Full Access", zh: "Full Access" }, capability: { feature: "permission-postures" }, body: { en: "Allows commands to reach anything on this computer. Managed policy and repository law still apply.", zh: "允许命令访问这台计算机上的任何内容。托管策略与仓库规则仍然适用。" } },
  ] satisfies ProductRow[],

  actions: {
    install: { en: "Install Codewhale", zh: "安装 Codewhale" },
    models: { en: "See every provider", zh: "查看所有提供商" },
    docs: { en: "Read the docs", zh: "阅读文档" },
  },
} as const;
