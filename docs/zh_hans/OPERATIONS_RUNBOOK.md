# Codewhale 运维手册

本手册介绍本地 CLI/TUI。下方命令依据当前源码编写；请先运行
`codewhale --version`，并对照已发布版本标签下的文档，不要假定候选版本的控制方式
已经包含在已发布版本中。

## 快速分诊

在发生问题的工作区先做离线检查：

```sh
codewhale --version
codewhale config path
codewhale doctor --json
```

`doctor --json` 报告结构性健康状况，不探测提供商凭据、不连接提供商，也不启动 MCP
服务器。离线报告不能证明密钥有效或远程服务可达。请勿把原始配置、密钥存储、会话归档
或环境变量转储附到公开 issue。

需要更多信息时，可以用 `RUST_LOG=codewhale_tui=debug codewhale` 复现。
日志和会话可能包含项目数据，分享前请检查并脱敏相关片段。
[排查指南](../INSTALL.md#13-troubleshooting) 介绍安装与网络报错。

## 事故：回合（turn）挂起或流停止

先查看对话记录中是否有等待审批、澄清问题或工具失败。如果 shell 命令仍在运行，
`Ctrl+B` 会将命令转到后台，回合继续；`/jobs` 可查看任务。需要时，请要求 Codewhale
停止指定任务；当前源码使用带 `task_id` 和 `cancel: true` 的 `task_shell_wait`，
不会把任务 id 当成操作系统进程号。

按 Esc 取消回合；如果弹窗先接收 Esc，请先关闭弹窗。中断回合不会撤销已经完成的文件
修改或外部操作。

托管路由可在明确同意联网检查后运行：

```sh
codewhale auth status --provider <provider-id>
codewhale doctor --probe-api
```

请把提供商 id 替换为所选提供商；`--probe-api` 检查当前路由，不一定是上一条命令中
指定的提供商。本地模型应使用 `codewhale doctor --probe-local`。这些探测会连接所选
服务，可能产生提供商用量，不属于上方的离线分诊。

恢复连接或修复凭据后再重试。重试可能重复外部操作，请先查看对话记录和
[收据](../RECEIPTS.md)。

## 事故：网络中断／离线行为

用 `/queue list` 查看尚未发送的消息。离线队列按会话保存在
`~/.codewhale/sessions/checkpoints/`；迁移时可能接收旧的全局 `offline_queue.json`。

恢复连接后，用 `/queue edit <n>` 查看某条消息，按 Enter 发送。请核对回复和队列状态，
不要假定排队任务已经自动执行。

## 事故：需要崩溃恢复

用 `codewhale sessions` 列出已保存的会话，再用 `codewhale resume <id>` 明确恢复，
或用 `codewhale --continue` 恢复工作区会话。TUI 中的 Ctrl+R 可打开会话选择器，
普通启动不会静默恢复已中断的会话。

恢复失败时，修复前请保留相关状态。较新的数据结构需要兼容的二进制文件，不要为了
消除报错而删除检查点。[旧路径指南](../LEGACY_PATHS.md) 介绍从 `~/.deepseek` 迁移。

## 事故：持久化状态的 schema 错误

记录完整报错与二进制版本。修改文件前先备份受影响的状态目录，再使用支持该结构的
二进制文件或有文档说明的迁移方式；请勿手动改写结构版本字段。

会话文件、Runtime 线程记录和 Fleet 日志是不同的存储。先确认报错指向哪一个，并保留
原始数据，以便复核或撤销修复。

## 事故：MCP／工具执行失败

检查全局或可信项目 `mcp.json` 中的服务条目及完整命令。手动运行或重新加载服务器会
执行它的代码，并可能连接其服务，请先审阅权限。使用 `/mcp` 诊断，检查策略或沙箱拒绝，
不要通过扩大权限来掩盖原因不明的失败。

修复现有配置前先备份。`codewhale mcp init --force` 会替换配置，不是无副作用的验证
命令；完成有意的修改后，用 `/mcp reload` 刷新连接。

## 事故后检查清单

记录执行的命令、完整输出、版本与最小复现工作区。在 bug 报告中附上脱敏诊断片段、
实际结果和预期结果，并区分经过验证的修复、临时绕过方式与未经验证的假设。
