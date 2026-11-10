# Fleet + Workflow 教程

> 英文原文：[FLEET_WORKFLOW_TUTORIAL.md](../FLEET_WORKFLOW_TUTORIAL.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

Fleet 和 Workflow 设计上要配合使用，但解决的是问题的不同部分：

- **Fleet** 负责配置并管理同一批智能体（sub-agent）：可复用的角色、模型路由、权限、日志、
  产物，以及状态/重启/停止控制。
- **Workflow** 描述编排：阶段、分支、归约、循环，以及可通过 fleet/子智能体运行时
  派发的 智能体叶子节点。

**默认的产品路径：** 用自然语言提需求。规模小或耦合紧密的工作，Work 会在
当前权限设置下直接处理。多步委派使用一份精简的 Workflow 计划：具名步骤、依赖、
受限范围和完成检查；结果与证据传递给需要它们的步骤。一个受限的独立任务可以
直接用后台智能体（agent）。要继续同一个智能体的工作，用 `followup`。后台任务运行期间，
输入框依然可用，普通的多智能体工作也不需要工作流文件。详见：
[Automatic Workflows](AUTOMATIC_WORKFLOWS.md)。

本教程讲的是**手动**的 fleet 任务规范 / 入库 Workflow 路径，面向需要持久宿主
worker 和可审阅规范的运维者。一句话的请求仍然不应该悄悄生成 `tasks.json`；
worker 卡片和权限设置让派发过程可见，又不必暴露编写机制。

示例使用 `codewhale fleet` 和 `/fleet`。磁盘路径、配置键和 Workflow 的
`--fleet` 标志都用 Fleet 这个名字。

## 1. 准备工作区

在你希望 worker 检查或修改的工作区里运行 fleet：

```sh
codewhale fleet init
```

这会在 `.codewhale/fleet.jsonl` 创建工作区账本。worker 日志和受限产物放在
`.codewhale/fleet/` 下；宿主适配器日志放在 `.codewhale/fleet-host/` 下。

如果想要具名的可复用 worker，打开 TUI 并运行：

```text
/fleet setup
```

选一个角色，决定这份配置是继承操作者路由，还是固定某个提供商（provider）/模型，再选配置
放在哪里（**This project** → `.codewhale/agents/<role>.toml`，或
**Personal** → `$CODEWHALE_HOME/agents/<role>.toml`，跨仓库可用，但同 id 的
项目配置仍是优先级更高的覆盖项），然后审阅确切的文件、权限/工具/路由设置，
并保存。保存控件会写明它的效果（"Save to this project" /
"Save as Personal profile"），替换已有文件时，一定会再确认一次。fleet 任务规范
可以用 `worker.agent_profile` 或更短的 `worker.profile` 别名引用任一解析出的
配置。

这样，fleet 定义就是跨仓库的，而不是某个运行中会话的权限来源。多仓库操作请从
共享父工作区启动 Codewhale。配置能用，不等于已经拿到文件系统访问权；会话的工作区、
显式受信任路径、信任模式和权限设置仍然拥有最终决定权。

## 2. 编写 fleet 任务规范

`codewhale fleet run` 接受 JSON 或 TOML。入库的
`docs/examples/fleet-dogfood.toml` 是贴近真实场景的手动冒烟示例；下面的 JSON
展示同样的编写形态，包含一个只读 reviewer 和一个受限的文档笔记 worker。
密钥与信任由 Runtime 的实时策略控制，fleet 身份两个都不带。

```json
{
  "name": "docs readiness check",
  "labels": {
    "kind": "tutorial"
  },
  "tasks": [
    {
      "id": "map-docs",
      "name": "Map current docs",
      "objective": "Find the docs that describe fleet and Workflow.",
      "instructions": "Read docs/FLEET.md and docs/WORKFLOW_AUTHORING.md. Report the command surfaces, current limitations, and any confusing gaps.",
      "worker": {
        "role": "reviewer",
        "tools": [
          "rg",
          "sed",
          "git"
        ]
      },
      "workspace": {
        "required_files": [
          "docs/FLEET.md",
          "docs/WORKFLOW_AUTHORING.md"
        ],
        "writable_paths": [],
        "environment": {
          "required": [],
          "allowlist": []
        }
      },
      "input_files": [
        "docs/FLEET.md",
        "docs/WORKFLOW_AUTHORING.md"
      ],
      "expected_artifacts": [
        "log",
        "report"
      ],
      "scorer": {
        "kind": "manual"
      },
      "retry_policy": {
        "max_attempts": 1
      }
    },
    {
      "id": "draft-gap-note",
      "name": "Draft gap note",
      "objective": "Draft a short local note for any missing tutorial steps.",
      "instructions": "Write a concise Markdown note with the missing fleet + Workflow tutorial steps. Do not edit public docs unless explicitly asked.",
      "worker": {
        "role": "builder",
        "tools": [
          "rg",
          "sed"
        ]
      },
      "workspace": {
        "required_files": [
          "docs/FLEET.md"
        ],
        "writable_paths": [
          ".codewhale/fleet"
        ],
        "environment": {
          "allowlist": []
        }
      },
      "expected_artifacts": [
        "log",
        "report"
      ],
      "scorer": {
        "kind": "manual"
      }
    }
  ]
}
```

把它保存为 `tasks.json`。这些 worker 继承当前提供商与模型；验证路由前请先连接提供商。
`--check` 检查规范和路由，不创建或启动运行，也不能证明凭据或远程模型有效。

常见的任务字段：

| 字段 | 用途 |
| --- | --- |
| `id`, `name` | 稳定的任务标识与显示名。 |
| `objective`, `instructions` | worker 的目标和确切的操作指令。 |
| `worker.role` | 内置或自定义的角色意图，例如 `reviewer`、`builder`、`read-only` 或 `smoke-runner`。 |
| `worker.profile` / `worker.agent_profile` | 已保存的 fleet 名册配置，从项目 `.codewhale/agents/`、个人 `$CODEWHALE_HOME/agents/` 或 `[fleet.profiles]` 解析。 |
| `worker.tools` | 该任务期望 worker 使用的工具名。 |
| `worker.model` | 首选的显式模型固定项。提供商/模型的校验仍由路由解析负责。 |
| `worker.model_class`, `worker.loadout` | 面向旧任务规范的兼容路由提示；新规范请优先用 `worker.profile` 加已保存配置里的路由固定项。 |
| `workspace.required_files` | 任务启动前必须存在的文件。 |
| `workspace.writable_paths` | 当前生效的运行时权限允许写入时，该任务可写的路径。 |
| `workspace.environment` | 必需或列入允许清单的环境变量，只按名字给出。 |
| `input_files`, `context` | 要串进任务提示词的额外文件和字符串。 |
| `expected_artifacts` | 期望出现的产物类型：`log`、`report`、`patch`、`test_result`、`checkpoint` 或 `receipt`。 |
| `scorer` | 确定性或人工的校验规则。 |
| `retry_policy`, `timeout_seconds`, `budget` | 重试与预算控制。 |

不要在新建的 fleet 任务规范里写 `security_policy` 或 worker 的 `trust_level`。
这些旧字段只有回放旧账本时还能读，新运行的校验会拒绝它们。项目信任、
文件系统/网络可达范围、密钥、审批、沙箱和工具权限，都是 Runtime 的策略输入。

## 3. 启动并监控 fleet

启动运行：

```sh
codewhale fleet run tasks.json --max-workers 4
```

命令会打印 run id 和 worker id。在另一个终端里监控账本状态：

```sh
codewhale fleet status
codewhale fleet inspect <worker-id>
codewhale fleet logs <worker-id>
codewhale fleet artifacts <worker-id>
```

worker 需要干预时，用带类型的控制命令：

```sh
codewhale fleet interrupt <worker-id>
codewhale fleet restart <worker-id>
codewhale fleet resume <run-id>
codewhale fleet stop --all
```

`resume` 用于 manager 退出、笔记本休眠或租约过期之后的重启恢复。它会回放账本，
把过期的工作对账处理掉，但不会创建新的运行。

## 4. 编写 Workflow

Workflow 源码是声明式 JavaScript 或 TypeScript，会被编译（lower）为带类型的 Rust
`WorkflowSpec`。它不是通用的 JavaScript 运行时：imports、进程访问、
文件系统读写、网络调用、`eval`、`async` 和 `await` 都会被拒绝。

创建一个入库文件，例如 `workflows/docs_readiness.workflow.js`。仓库里还有一个
持续维护的示例 `workflows/issue_audit.workflow.js`。

```js
export default workflow({
  "id": "docs-readiness",
  "goal": "Inspect fleet and Workflow docs, then synthesize a readiness note",
  "nodes": [
    {
      "branch": {
        "id": "parallel-docs-audit",
        "parallel": true,
        "children": [
          {
            "agent": {
              "id": "fleet-docs",
              "prompt": "Inspect docs/FLEET.md for command and task-spec coverage.",
              "agent_type": "review",
              "mode": "read_only",
              "profile": "reviewer",
              "file_scope": ["docs/FLEET.md"]
            }
          },
          {
            "agent": {
              "id": "workflow-docs",
              "prompt": "Inspect docs/WORKFLOW_AUTHORING.md for Workflow authoring coverage.",
              "agent_type": "review",
              "mode": "read_only",
              "profile": "reviewer",
              "file_scope": ["docs/WORKFLOW_AUTHORING.md"]
            }
          }
        ]
      }
    },
    {
      "reduce": {
        "id": "readiness-summary",
        "inputs": ["fleet-docs", "workflow-docs"],
        "prompt": "Summarize the exact docs gaps and the safest next edit."
      }
    }
  ]
});
```

当前的 Workflow 节点包装器有 `agent`、`branch`、`sequence`、`reduce`、
`teacher_review`、`loop_until`、`cond` 和 `expand`。`agent.profile` 指定一个
fleet 名册配置；显式的 agent 字段会覆盖配置里的默认值。

面向模型的 `workflow` 工具可以用内联源码或 `source_path` 启动、运行、检查或
取消一个工作流。当 Codewhale 走这条路径时，要是该工作流会启动多个 worker 或
改动文件，就先让它把计划展示出来。

## 5. 自然语言入口

目前一句好用的提示词是：

```text
Draft a fleet task spec for this goal, but do not run it yet.
Show the proposed tasks, worker profiles, writable paths, expected artifacts,
scorers, and security policy. Keep secrets disabled unless I explicitly grant
them.
```

审阅生成的规范之后，把它保存为 `tasks.json`，再运行上面的 fleet 命令。
对工作流，请让 Codewhale 起草一个 `.workflow.js` 文件、展示计划，
并且只在批准之后再走 workflow 工具路径。

这一步审阅是有意设计的。它会在启动持久 worker 之前，把提供商路由、DeepSeek 或其他
模型支持、可写路径、网络访问和密钥使用都摆到明面上。
