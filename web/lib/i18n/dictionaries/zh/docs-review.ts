import type { DocsReviewDict } from "../types";

/** 「查看改动」页的简体中文词典；与 `en/docs-review.ts` 逐段对应。 */
export const docsReview: DocsReviewDict = {
  metaTitle: "查看改动 · Codewhale 文档",
  metaDescription:
    "查看 Codewhale 在一次会话中改过的每个文件，把工作区回滚到之前的回合，对 diff 做代码审查，并为审查保留一份回执。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "查看改动",
  lede:
    "Codewhale 每改一处都会当场展示给你，并在每个回合前后为工作区保存快照。这一页介绍如何查看改了什么、把文件恢复原样，以及在推送之前请它再审一遍。",
  sections: [
    {
      "id": "receipts",
      "title": "查看会话做了什么",
      "blocks": [
        {
          "p": "会话收据列出已记录的文件修改、命令、工具、智能体、审批与失败。下方的审查收据记录单独的代码审查。"
        },
        {
          "code": "/receipts\n/receipts json\ncodewhale receipts --last --format md\nGET /v1/threads/{id}/receipt\nGET /v1/threads/{id}/turns/{turn_id}/receipt",
          "lang": "Codewhale / Terminal / HTTP"
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
        { p: "每次修改文件，都会以 diff 的形式出现在对话记录里。想一次看全，运行：" },
        { code: "/diff", lang: "Codewhale" },
        {
          p: "`/diff` 会显示本次会话开始以来的全部改动。从 0.10.2 起，它会与本次会话的第一个还原点比较，因此在不是 git 仓库的文件夹里也能使用，并在分页器中打开。你自己的 git 历史不受影响，`git diff` 和 `git status` 照常可用。",
        },
      ],
    },
    {
      id: "restore",
      title: "把文件回滚到之前的回合",
      blocks: [
        {
          p: "每个回合开始前和结束后，Codewhale 都会把工作区快照存进一个独立的 git 存储。它从不写入你仓库自己的 `.git`，在不是 git 仓库的文件夹里也能用。",
        },
        {
          code: `/restore           # list the 20 most recent snapshots
/restore list 50   # list more (up to 100)
/restore 1         # put files back to the newest snapshot`,
          lang: "Codewhale",
        },
        {
          p: "恢复会改动文件，所以需要工作区已被信任（`/trust on`）或处于 Full Access；查看快照列表则不受限制。你也可以直接说“撤销你刚才的修改”，Codewhale 可以回滚它自己的那个回合。",
        },
        {
          p: "`/undo` 撤回你上一次的请求。在 0.10.1 中，恢复文件需要先运行 `/trust on`，并且每次只回退一次工具调用。从 0.10.2 起，它在任何权限模式下都会恢复该请求改动的每个文件，并从对话中删除该请求及其回复。`/restore` 会把整个工作区回滚到更早的快照。",
        },
        {
          note: "快照保留 7 天。超过 2 GB 的工作区不做快照，Codewhale 会提示你一次；如果仍想要快照，可以在配置中调大 `[snapshots] max_workspace_gb`。如果没有安装 git 或磁盘已满，回合照常运行，只是不做快照。",
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
          p: "审查回执记录了审查了什么、发现了什么，让你能证明推送出去的 diff 正是被审查过的那一份。",
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
/export file notes/session.md      # the same, written to a file
codewhale sessions export <id>     # full archive: messages, tool calls, results, artifacts`,
          lang: "Codewhale / 终端",
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
