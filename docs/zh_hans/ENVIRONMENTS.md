# 特定环境的注意事项

> 英文原文：[ENVIRONMENTS.md](../ENVIRONMENTS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

标准的构建/测试/运行命令放在 `AGENTS.md` 和 `CONTRIBUTING.md`。本文件只记录
特定环境里那些不明显的怪癖，免得在永远碰不到它们的机器上占用上下文。

## Cursor Cloud 虚拟机

- **系统构建依赖：** 构建需要 `libdbus-1-dev`（由 `crates/secrets` 为 OS
  密钥环引入）。它由启动更新脚本安装；如果 `cargo build` 报 `dbus`/`pkg-config`
  错误，就是缺这个依赖。
- **必须设置 `rustup default`：** 有些测试和运行时（runtime）路径会在本检出
  目录*之外*的临时目录里拉起 shell（例如 `run_verifiers_background_*`、
  子智能体（subagent）工作树）。这些被拉起的 shell 只有在 `/workspace` 内部
  才能看到仓库的 `rust-toolchain.toml` 覆盖设置，所以没有全局默认值时，
  它们会以“rustup could not choose a version of rustc to run”失败。
  更新脚本会运行 `rustup default stable` 来修复这一点。
- **`/workspace` 上已知的环境相关测试失败（不是代码缺陷）：** 因为检出目录
  直接位于 `/` 之下，有两个 `codewhale-tui` 子智能体测试会在这里失败——
  `git_repo_root_reports_attempted_paths_when_no_repo_found`（无法在不可写的
  父目录 `/` 里创建临时目录）和
  `create_isolated_worktree_reports_friendly_error_when_no_repo_found`
  （向上遍历到 `/` 时会把 `/workspace` 本身当成仓库）。当仓库检出到一个正常
  可写的父目录下时，这两个测试都会通过。

## 在没有提供商 API key 的情况下运行智能体

通过免密钥的 `vllm`/`ollama`/`sglang` 提供商（provider），把 Codewhale
指向任意本地 OpenAI 兼容端点：

```sh
CODEWHALE_PROVIDER=vllm VLLM_BASE_URL=http://127.0.0.1:8000/v1 VLLM_MODEL=<id> \
  codewhale exec --auto "..."
```

`codewhale exec`（加上 `--auto` 可启用工具调用）是跑通完整智能体循环的非交互路径。
`--auto` 默认启用 Full Access（完全访问）：没有 Codewhale shell 沙箱，并开启信任模式。
加上 `--sandbox workspace-write` 可保留 shell 沙箱和文件工具的工作区限制，并关闭信任模式。

## Linux 沙箱终端

沙箱 PTY 需要 bubblewrap，以及位于 `/usr/bin/setsid` 或 `/bin/setsid` 的
util-linux `setsid`。Codewhale 在 bubblewrap 的 PID 命名空间内分配控制终端，
让 Ctrl-C 发送到前台程序而不终止包装进程。处理 SIGINT 的程序仍可继续接收输入。
`exec_shell` PTY 任务和持久终端会话共用此路径。缺少 `setsid` 会拒绝 PTY 启动，
不会移除沙箱；非 PTY 命令不需要该辅助程序。命名空间隔离、只读挂载和
`--die-with-parent` 清理仍然生效。

## 在回合期间让主机保持唤醒

交互式 TUI 回合（turn）进行中时，Codewhale 会持有平台的空闲休眠断言，
这样无人值守的机器不会在回合中途空闲入睡而丢掉工作：

- macOS：`caffeinate -i`
- Linux：`systemd-inhibit --what=idle --why="Codewhale turn in flight" --mode=block cat`，
  其中 `cat` 读取的管道由 Codewhale 在回合期间持有

断言在回合结束的那一刻释放——在 Linux 上通过关闭那条管道实现，于是 `cat`
退出、`systemd-inhibit` 随之结束，不会留下残留进程——而且它只覆盖*空闲*休眠：
显式执行 `sleep` / `pmset sleepnow`、合上盖子或电量过低仍会让机器挂起。
无头主机——`exec`、app-server、CI——从不持有它，所以共享 runner 的电源策略
不受影响。Windows 未实现：`SetThreadExecutionState` 是线程亲和的，需要一个
固定线程的持有者，所以这个缺口是有意为之，而不是被悄悄忽略。

如果回合还是被挂起了，引擎会在唤醒时察觉——墙上时钟耗时与单调时钟耗时之差
超过挂起阈值——上报 `System sleep detected; connection lost — retrying request`，
并重新发起请求，而不是让回合失败（#2990）。

## Windows 支持与发布验收

原生 x64 CLI/TUI 的兼容性目标是 Windows 10 1607（内部版本 14393）及以上，
在传统 Windows 控制台中运行。静态导入检查通过，不等于最终发布文件已经在这台
旧系统上启动或完成安装；必需的 Visual C++ 运行库 DLL 也必须存在。

| 使用场景 | 目标或平台前提 | 验收边界 |
| --- | --- | --- |
| 原生 x64 CLI/TUI | Windows 10 x64 1607，内部版本 14393 | 最终发布文件仍需真实旧系统启动回执 |
| ConPTY API | Windows 10 1809，内部版本 17763；Windows Server 2019 | 这是微软的 API 前提；Codewhale 终端会话接口仍仅限 Unix，Windows 返回 501 |
| 当前 Windows Terminal | Windows 10 2004，内部版本 19041，或更高版本 | 它的前提比 ConPTY API 更新；x64 旧系统兼容性目标使用传统控制台 |
| 原生 ARM64 CLI/TUI | 目标是 Windows 11 ARM64 | 导出在内部版本 26300（debug 候选）和 26200（发布构建，托管探测）上得到验证；不能由此推断最低 Windows 11 版本或 Windows 10 ARM64 支持 |
| NSIS 安装器 | x86 安装程序和插件在 WOW64 下运行，安装 x64 程序 | ARM64 使用原生压缩包/npm；生成的 Uninstall.exe 与实际安装行为需独立验收 |

[ConPTY API 前提](https://learn.microsoft.com/en-us/windows/console/createpseudoconsole)
和[当前 Windows Terminal 前提](https://github.com/microsoft/terminal/blob/main/README.md)
应分别说明。Windows 11 ARM64 上的 x64 模拟运行不能验证原生 x64 或 Windows 10 1607。

[`发布导入基线`](../../scripts/release/windows-import-floor.json)按 PE 架构分别记录。
未知导入、未列入基线的序号导入和延迟加载依赖会阻止发布门槛通过。PASS 只表示已
核对的静态导入契约，不认证整个运行时、安装依赖或最低 OS 版本。Native 扩展宿主
仍需要对应的运行时能力 API 和隔离检查；缺少 DeriveCapabilitySidsFromName 时明确
返回 Unsupported，不会降低沙箱隔离。此限制与 CLI 启动兼容性不同。

若候选版本的 windows-arm64 导入检查因 arm64 基线未见过的导入而失败，用
[`windows-arm64-export-probe.yml`](../../.github/workflows/windows-arm64-export-probe.yml)
重新验收：以该候选运行的 `run_id` 和 `source_sha` 手动触发。它在 GitHub 托管的
Windows 11 ARM64 运行器上，于原生 ARM64 进程中从 System32 加载每个导入的 DLL，用
GetProcAddress 解析每一对导入，并上传记录模块路径与哈希的探测 JSON。只把探测解析成功
的导入加入基线，在其验收记录中写明运行、运行器系统版本、程序 SHA256 与数量，然后重新
运行候选版本。该探测只做静态导出解析，不启动程序。

2026-10-13 的候选源代码为
[`cf7199b52e`](https://github.com/codewhale-hq/Codewhale/commit/cf7199b52ecb9147e4670ae7874924ffc30a6a67)。
在 Windows 11 ARM64 内部版本 26300 上，默认功能 debug 程序的 SHA256 为
`ca51f91562a0569ee43855715856e1cfee74e8a834bd6206183a3175373fdb2d`；原生 ARM64
进程只从系统目录查询 DLL，453 个导入全部解析成功。普通用户环境中 89 个命令分类
测试和 1 个崩溃恢复测试通过，并完成了限定范围的编辑器滚轮、拼音与撤销检查。
它不是 dist 发布程序或安装验收，也不能据此指定 Windows 11 的最低内部版本。

x64 基线来自 @LmeSzinc 在
[#6953](https://github.com/codewhale-hq/Codewhale/issues/6953)中对 14393.447 的调查：
v0.10.0 的 411 个导入全部成功，v0.10.1 的 454 个中有 453 个成功。缺失的
DeriveCapabilitySidsFromName 不在允许列表中；两次调查的保守并集为 456 对导入。
x86 NSIS 基线有 189 个微软文档标明早于 1607 的命名 API，以及明确核对的
COMCTL32 #17（InitCommonControls）：x86 SDK 10.0.26100.0 导入库与系统 SysWOW64
DLL 的身份一致，[该 API 的文档前提是 Vista](https://learn.microsoft.com/en-us/windows/win32/api/commctrl/nf-commctrl-initcommoncontrols)。
基线文件包含对应的官方 URL、文件哈希、数量与序号身份；这不等于在 1607 上安装成功。

2026-10-16，探测运行 38890634267（Windows 11 ARM64 内部版本 26200.9457）解析了候选运行
38867608774 中 windows-arm64 发布程序的全部 445 个导入，2026-10-13 基线未见过的 7 对导入
已加入。随后冻结的 0.10.2 源代码 `14683c9983` 上的发布候选 38899652987 在所有 Windows
目标上通过导入检查：ARM64 445 个，x64（含安装器内程序）453 个，x86 安装程序 163 个。

此快照中，最终发布文件在真实 1607 上启动/安装及候选程序生成的卸载器仍未验收。
最终冻结源码、发布导入记录、各安装途径、打包/原生应用崩溃恢复和最低 OS 回执应
进入发布验收记录，见[英文发布检查单](../RELEASE_CHECKLIST.md)。

## Windows PowerShell 执行策略

shell 工具以 `-ExecutionPolicy Bypass` 运行 PowerShell。它只设置所启动子进程
自己的策略：不会持久化，不需要管理员权限，你自己打开的 PowerShell 窗口仍保持
原有策略。没有它时，本地策略为 `Restricted`（Windows 客户端的默认值）或
`AllSigned` 的机器，会拒绝运行 Codewhale 为多行命令写入的临时 `.ps1` 脚本
（#6745）。

由组策略设置的策略（`Get-ExecutionPolicy -List` 中 `MachinePolicy` 或
`UserPolicy` 行）优先级高于进程范围。在这样的机器上，多行命令仍会被拒绝，
PowerShell 的拒绝信息会作为该命令的错误返回；Codewhale 不会绕过管理员强制的
策略。单行命令通过 `-Command` 运行，不受执行策略约束。命令自身调用的脚本也在
同一进程范围内运行；决定什么可以运行的是 shell 工具的审批与沙箱设置，而不是
执行策略。

若希望改由机器或用户策略生效，请在启动 Codewhale 之前设置
`CODEWHALE_POWERSHELL_EXECUTION_POLICY=inherit`：此时 shell 工具会完全省略
`-ExecutionPolicy`，因此策略为 `Restricted` 或 `AllSigned` 时，需要临时 `.ps1`
脚本的多行命令会被拒绝。未设置、设置为 `bypass` 或任何其他值时，仍保持默认的
`Bypass`。

## 统一的运行时命令

当前的 `codewhale` 二进制在进程内运行 TUI。发布安装器会把同样的字节复制到
可选的 `codew` 短命令；不需要另外的 `codewhale-tui` 可执行文件。
`DEEPSEEK_TUI_BIN` 仍是遗留的回放/迁移设置，不是当前安装所必需的。
