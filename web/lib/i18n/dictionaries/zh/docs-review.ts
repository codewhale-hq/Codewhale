import type { DocsReviewDict } from "../types";

/** 「查看改动」页的简体中文词典；与 `en/docs-review.ts` 逐段对应。 */
export const docsReview: DocsReviewDict = {
  metaTitle: "查看改动 · Codewhale 文档",
  metaDescription:
    "查看已记录的改动、恢复可用的工作区快照，并在推送前进行代码审查。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "查看改动",
  lede:
    "查看改动、恢复可用的工作区快照，并在推送前再做一次审阅。",
  sections: [
    {
      "id": "receipts",
      "title": "查看会话做了什么",
      "blocks": [
        {
          "p": "用 [/preview-request](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/PREVIEW_REQUEST.md) 检查下一次主要模型请求，无需发送。"
        },
        {
          "p": "会话收据列出修改的文件、运行的命令以及每一步由谁批准，并在末尾列出它没有记录的内容。收据不显示费用或 token 行：页脚和 `/cost` 会显示费用数字，或显示“未知”。下方的审查收据记录单独的代码审查。"
        },
        {
          "code": "/receipts\n/receipts json",
          "lang": "Codewhale"
        },
        { code: "codewhale receipts --last --format md", lang: "Terminal" },
        {
          code: "GET /v1/threads/{id}/receipt\nGET /v1/threads/{id}/turns/{turn_id}/receipt",
          lang: "HTTP"
        },
        {
          "p": "这些接口只读取记录，不会调用提供商、运行工具或修改会话状态。"
        },
        {
          "note": "Runtime 收据不会逐项列出 shell 命令修改的文件。终端可根据回合快照报告修改；较旧记录可能缺少审批决策者。"
        }
      ]
    },
    {
      id: "diff",
      title: "查看改动",
      blocks: [
        { p: "文件工具的修改会以 diff 形式出现在对话记录里。想一起检查可用的会话改动，可以运行：" },
        { code: "/diff", lang: "Codewhale" },
        {
          p: "从 v0.10.2 起，`/diff` 在可用时会使用会话的第一个还原点，因此也可用于没有 git 仓库的文件夹。快照未捕获的路径不在比较范围内；你自己的 `git diff` 与 `git status` 仍使用原仓库。",
        },
      ],
    },
    {
      id: "restore",
      title: "把文件回滚到之前的回合",
      blocks: [
        {
          p: "可用的工作区快照单独存储，不会写入你仓库的 `.git`。文件夹本身不是 git 仓库也能使用快照。",
        },
        {
          code: `/restore           # list the 20 most recent snapshots
/restore list 50   # list more (up to 100)
/restore 1         # put files back to the newest snapshot`,
          lang: "Codewhale",
        },
        {
          p: "恢复会改动文件，所以需要工作区已被信任（`/trust on`）或处于 Full Access；查看快照列表则不受限制。也可以直接要求 Codewhale 撤销上一次改动。",
        },
        {
          p: "`/undo` 撤回上一次请求及其记录的文件改动。还原点不可用或文件后来又被修改时，会拒绝恢复文件。`/restore` 用于选择工作区快照。",
        },
        {
          note: "快照默认保留 7 天，按数量清理时会保留近期回合的还原点；超过 2 GB 的工作区会跳过快照，除非调整 `[snapshots] max_workspace_gb`。被忽略或跳过的文件以及外部影响不在完整备份范围内；git 或存储不可用时，回合可能在没有快照的情况下运行。",
        },
      ],
    },
    {
      id: "code-review",
      title: "做一次代码审查",
      blocks: [
        {
          p: "在会话中，`/review` 可以对一个文件、一段 diff 或一个拉取请求做结构化审查。在 shell 中，`codewhale review` 会审查一段 git diff 并输出发现的问题：",
        },
        {
          code: `codewhale review                      # unstaged changes in the working tree
codewhale review --staged             # what you are about to commit
codewhale review --base origin/main   # everything on this branch
codewhale review --pr 123             # a GitHub pull request (needs gh)`,
          lang: "终端",
        },
        {
          p: "加上 `--path <file>` 只审查一个路径，用 `--model` 指定审查模型，用 `--json` 输出机器可读结果。超过 200,000 个字符的 diff 会被直接拒绝，而不是截断；需要时可以用 `--max-chars` 调高上限。",
        },
        {
          note: "`--post` 会把审查结果作为评论发布到拉取请求上。不加这个参数时，除了发给模型的请求，没有任何内容离开你的终端。",
        },
      ],
    },
    {
      id: "review-receipts",
      title: "为审查保留回执",
      blocks: [
        {
          p: "审查回执记录已审查 diff 的指纹及报告的发现，供你检查之后是否又有变化。它不能证明审查找出了所有缺陷，也不能证明改动安全。",
        },
        {
          code: `codewhale review --base origin/main --write-receipt
codewhale review --base origin/main --check-receipt`,
          lang: "终端",
        },
        {
          list: [
            "`--write-receipt` 在审查成功后保存一个本地 JSON 文件：diff 的指纹、提供商与模型、各类发现的数量、未解决的风险，以及审查文本的哈希。它不保存 diff 本身。",
            "`--check-receipt` 不调用模型。如果 diff 在回执生成后发生了变化、审查留下了未解决的风险，或者附带的检查没有通过，它都会以非零状态退出，因此可以用作推送前的检查关卡。",
            "`--receipt-path <file>` 写入或读取指定的回执，而不是最新的那一份。",
          ],
        },
      ],
    },
    {
      id: "export",
      title: "保存整个会话",
      blocks: [
        { p: "想保留或分享一次会话的完整记录，可以导出：" },
        {
          code: `/export clipboard                  # a redacted copy of this session
/export file notes/session.md      # the same, written to a file`,
          lang: "Codewhale",
        },
        { code: "codewhale sessions export <id>", lang: "终端",
        },
        {
          p: "`codewhale sessions` 会列出已保存的会话及其 id。完整归档包含全部上下文，请像对待源代码一样对待它。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "决定 Codewhale 修改或运行任何东西之前是否先问你。",
    },
    {
      href: "/docs/fleet",
      label: "运行工作流",
      note: "可重复的多步骤工作，每次运行都有记录。",
    },
    {
      href: "/docs/troubleshooting",
      label: "排查问题",
      note: "回合卡住、密钥失效或会话无法恢复时该怎么办。",
    },
  ],
  sourceNote:
    "来源文档：docs/RECEIPTS.md、docs/CONFIGURATION.md（[snapshots]）、crates/snapshot/src/lib.rs · 修改时同步更新 docs-map.ts。",
};
