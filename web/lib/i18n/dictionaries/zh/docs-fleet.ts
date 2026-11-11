import type { DocsFleetDict } from "../types";

/** 「运行工作流」页的简体中文词典；与 `en/docs-fleet.ts` 逐段对应。 */
export const docsFleet: DocsFleetDict = {
  metaTitle: "运行工作流 · Codewhale 文档",
  metaDescription:
    "在 Fleet 中保存角色和模型，编写可重复的工作流，把它作为可查看、可停止的 Lane 运行，并用持久的智能体批量执行任务。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "运行工作流",
  lede:
    "在 Work 中，你可以请 Codewhale 规划较大的任务，把独立步骤交给智能体。如果你希望重复执行包含阶段和并行分支的有序计划，并保留每次运行的记录，就编写 Workflow。",
  sections: [
    {
      id: "fleet",
      title: "在 Fleet 中保存角色",
      blocks: [
        {
          p: "Fleet 保存可复用的角色和模型选择。先连接模型，再在会话中创建角色配置：",
        },
        { code: "/fleet setup\n/fleet\n/fleet saved", lang: "Codewhale" },
        {
          p: "`/fleet setup` 每次保存一个角色配置。选择模型或沿用会话路由，再选择当前项目或个人配置。保存前先审阅完整文件。`/fleet` 显示当前团队；`/fleet saved` 用于切换已有的具名团队。接着用自然语言交代任务，并用 `/fleet workers` 查看该会话的智能体。",
        },
        {
          p: "Fleet 只决定由谁来做。一个智能体能读、能写、能运行什么，仍然取决于你的工作区信任、[审批设置](/docs/modes)和沙箱。",
        },
      ],
    },
    {
      id: "write",
      title: "编写工作流",
      blocks: [
        {
          p: "保存的 Workflow 描述智能体可以重复执行的流程。创建 `workflows/` 文件夹，将这个声明式示例保存为 `workflows/docs_readiness.workflow.js`。它先并行审查两个部分，再汇总结论。请把 `src` 和 `docs` 换成你的项目中确实存在的目录：",
        },
        {
          code: `export default workflow({
  "id": "docs-readiness",
  "goal": "Review the docs and code for gaps, then summarize the next edit",
  "nodes": [
    {
      "branch": {
        "id": "parallel-review",
        "parallel": true,
        "children": [
          { "agent": { "id": "code-review", "prompt": "Inspect src/ for undocumented behavior.",
                       "agent_type": "review", "mode": "read_only", "file_scope": ["src"] } },
          { "agent": { "id": "docs-review", "prompt": "Inspect docs/ for stale or missing steps.",
                       "agent_type": "review", "mode": "read_only", "file_scope": ["docs"] } }
        ]
      }
    },
    {
      "reduce": {
        "id": "summary",
        "inputs": ["code-review", "docs-review"],
        "prompt": "Combine the findings into the safest next edit."
      }
    }
  ]
});`,
          lang: "workflows/docs_readiness.workflow.js",
        },
        {
          p: "这种声明式写法支持 `agent`、`branch`、`sequence`、`reduce`、`loop_until`、`cond`、`expand` 和 `teacher_review`。它会编译成经过验证的计划，拒绝 `import`、`fetch`、`eval`、`async` 等语法与调用。高级命令式 JavaScript 可在沙箱中通过异步 `task()` 和 `parallel()` 协调工作，但仍不能直接访问文件、shell 或网络；这些操作由智能体按当前策略执行。",
        },
        {
          note: "Workflow 会限制同时运行的智能体数量；已接纳的工作等待空闲位置。默认不限制累计子智能体数量或计划结构深度，可用正数的 `[workflow] max_children` 或 `max_depth` 设置上限。运行时的智能体接纳和委派限制另行生效。循环必须声明 `max_iterations`。",
        },
      ],
    },
    {
      id: "run",
      title: "运行",
      blocks: [
        {
          code: `codewhale workflow run docs-readiness --runtime inline`,
          lang: "终端",
        },
        {
          p: "该命令找到 `workflows/docs_readiness.workflow.js`，验证后启动需要模型的运行，因此请先连接提供商。`--runtime inline` 在当前终端等待完成。默认后端要求主机已安装 `tmux`，并在独立会话中运行。启动时加上 `--verify`，可在成功完成后执行已配置的验证关卡；`--fleet <name>` 选择已有的具名 Fleet。",
        },
        {
          p: "如果不想动到当前的工作副本，加上 `--worktree-repo . --branch <name>`：这次运行会拥有自己的 git 工作树和分支。",
        },
        {
          p: "在会话中，`/workflow <objective>` 只起草方案，不执行。要审阅这个已保存的文件，请用 `/workflow run workflows/docs_readiness.workflow.js`；等待审阅完成，再用 `/workflow confirm` 启动已审阅的方案。`/workflow status` 读取运行状态，`/workflow cancel <run-id>` 取消运行，`/workflows` 打开会话运行面板。",
        },
      ],
    },
    {
      id: "watch",
      title: "查看和停止运行",
      blocks: [
        { p: "每次运行都是一个 Lane。Lane 会保存到磁盘上，所以你可以在任何终端里查看：" },
        {
          code: `codewhale lane list
codewhale lane status <lane-id>
codewhale lane logs <lane-id>
codewhale lane attach <lane-id>
codewhale lane interrupt <lane-id>`,
          lang: "终端",
        },
        {
          p: "`lane list`、`lane status` 和 `lane interrupt` 支持 `--json`。会话内可用 `/lane list`、`/lane status <lane-id>` 或 `/lane interrupt <lane-id>`；查看日志和连接 tmux 使用上面的 shell 命令。斜杠读取显示最近记录的状态，CLI list/status 则会核对后端退出情况。斜杠 interrupt 先返回排队回执，再返回最终结果。",
        },
      ],
    },
    {
      id: "batch",
      title: "批量运行任务",
      blocks: [
        {
          p: "如果你手上是一串彼此独立的任务，而不是一套计划，就把它们写成任务文件，作为一次 Fleet 运行来执行。每个任务写明目标、角色和允许写入的路径。完整的 `tasks.json` 示例见[教程](/docs/workflow-tutorial)。",
        },
        {
          code: `codewhale fleet init
codewhale fleet run tasks.json --check
codewhale fleet run tasks.json --max-workers 4
codewhale fleet status
codewhale fleet logs <worker-id>
codewhale fleet resume <run-id>
codewhale fleet stop --all`,
          lang: "终端",
        },
        {
          p: "`fleet run --check` 只验证，不创建运行；去掉 `--check` 才启动前台管理进程。运行期间，在另一个终端查看状态和操作控制。中断后，`fleet resume <run-id>` 核对过期租约并报告状态，不会重启管理进程或启动排队工作。若要明确重试某个智能体，先 inspect，再用 `codewhale fleet restart <worker-id>`，它会为该次运行启动管理进程。`/fleet workers` 和 `/subagents` 显示当前会话智能体，不显示这些持久运行。",
        },
      ],
    },
  ],
  next: [
    { href: "/docs/workflow-tutorial", label: "阅读完整 Fleet 教程", note: "启动 worker 前，验证完整任务文件并了解恢复行为。" },
    {
      href: "/docs/subagents",
      label: "并行运行智能体",
      note: "不写工作流，也能把一个任务中彼此独立的部分交给子智能体。",
    },
    {
      href: "/docs/review",
      label: "查看改动",
      note: "检查一次运行产生的 diff，并在推送前做一次审查。",
    },
    {
      href: "/docs/vocabulary",
      label: "产品名词",
      note: "用一句话分别说明 Fleet、Workflow、Lane 和 Runtime。",
    },
  ],
  sourceNote:
    "来源文档：docs/FLEET.md、docs/FLEET_WORKFLOW_TUTORIAL.md、docs/WORKFLOW_AUTHORING.md · 修改时同步更新 docs-map.ts。",
};
