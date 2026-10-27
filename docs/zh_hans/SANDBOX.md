# 沙箱威胁模型

> 英文原文：[SANDBOX.md](../SANDBOX.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

Codewhale 可以执行由模型提出的 shell 命令。审批策略、感知工作区的工具，
以及操作系统层面的命令包装器，这三套是彼此独立的控制手段：一次审批不等于沙箱，
选择 `workspace-write` 也不代表当前平台真的提供了可用的操作系统包装器。

本文只描述已经接入命令执行路径的行为。至于执行到达这条边界之前会先经过哪些
策略层，见 [授权顺序（Authorization order）](AUTHORIZATION_ORDER.md)。

## 平台概览

| 机制 | 平台 | 选择方式 | Codewhale 报告的结果 |
|---|---|---|---|
| Seatbelt（`sandbox-exec`） | macOS | 运行时探测成功时自动启用 | `macos-seatbelt` |
| Bubblewrap（`/usr/bin/bwrap`） | Linux | 默认启用，前提是已安装且探测包装运行成功；`prefer_bwrap = false` 可退出 | `linux-bwrap` |
| 无操作系统包装器 | Linux，bwrap 不可用或已退出 | `prefer_bwrap = false`，或 bwrap 不存在/不可用 | `none` |
| 无操作系统包装器 | Windows | 当前实现 | `none` |
| 兼容 OpenSandbox 的服务 | 任何受支持的主机 | `sandbox_backend = "opensandbox"` | 外部执行路径 |

仓库里有一份面向未来的 Windows 辅助程序契约（helper contract）。它没有接入
子命令的启动流程，所以 Codewhale 不会对外声明它是生效中的沙箱。光有沙箱
源码，不能证明某条命令真的被限制过。

## macOS：Seatbelt

Codewhale 用一个最小 profile 探测 `/usr/bin/sandbox-exec`。当探测通过、
并且选定的 `SandboxPolicy` 要求沙箱时，子命令外面会套上一层生成的
Seatbelt profile。

这层 profile 可以提供：

- 大范围的文件系统读取；
- 写入受所选策略限制，范围包括工作区，以及受支持工具所需的特定运行时/缓存路径；
- 只有在策略允许时才放开网络访问。带网络的 `read-only` 策略（智能体的
  `read_only: true` shell 模式）只获得出站 IPv4 和系统 DNS 解析器。IPv6 以及本机
  自己的地址（包括回环地址）都被拒绝，只有 `HTTP_PROXY`、`HTTPS_PROXY` 或
  `ALL_PROXY` 指定的本地代理服务器端口例外；其他本地套接字仍被拒绝。

探测失败，或 `sandbox-exec` 不可用时，Codewhale 会报告未启用操作系统沙箱，
直接启动命令，不套 Seatbelt 包装器。这条回退路径上也不会打任何 Seatbelt 标记。

## Linux：默认启用的 bubblewrap

只要 bubblewrap 可用，Linux 下的命令沙箱默认启用。退出使用顶层配置项：

```toml
prefer_bwrap = false
```

只有当 `/usr/bin/bwrap` 是普通的可执行文件，且一次实际的包装探测运行证明
它能在这台主机上创建命名空间时，Codewhale 才会选用 bubblewrap —— 仅有可执行
位会骗人：在限制用户命名空间的主机上（例如开启了 `kernel.apparmor_restrict_unprivileged_userns`
的 Ubuntu 24.04），每条被包装的命令都会失败，而不是以未沙箱化方式运行。
包装器根据解析后的 `SandboxPolicy` 推导自己的挂载点和网络命名空间：

```text
/usr/bin/bwrap \
  --unshare-all \
  [--share-net] \
  --ro-bind / / \
  --dev /dev \
  --proc /proc \
  --tmpfs /tmp \
  [--dev-bind <device-root> <device-root> ...] \
  --bind <writable-root> <writable-root> ... \
  --ro-bind <protected-descendant> <protected-descendant> ... \
  [--ro-bind <extra-ro-root> <extra-ro-root> ...] \
  --chdir <cwd> \
  -- <program> <args>
```

沙箱总会拿到私有的 `/dev`（全新的设备节点，所以 `>/dev/null` 照常可用）、
私有的 `/proc`，以及 tmpfs 挂载的 `/tmp`（#5410）。还有两个可选的顶层配置项
可以扩展挂载：`bwrap_ro_roots` 把额外的主机路径以只读方式 bind mount 进来，
最后才应用，因此能够收窄策略允许写入的路径；`bwrap_dev_roots` 把主机的
字符/块设备节点以读写方式 bind mount 进来，目录一律不予采纳。路径不存在时
静默跳过。

这样，子进程看到的是一个只读的根视图。在 `workspace-write` 下，每一个安全且
确实存在的策略根都会以读写方式挂载：工作目录、配置的额外根目录、未被排除的
`/tmp` 和 `TMPDIR`，以及经过校验的 Git worktree 元数据根。已经存在的
`.codewhale` 和 `.deepseek` 子路径，会在可写父目录挂好之后重新挂为只读。不存在的
路径、非目录路径以及 `/`，都不会被提升为可写挂载。

在 `read-only` 下没有任何可写绑定，所以工作目录仍留在只读根视图里。
`--unshare-all` 默认隔离网络命名空间；只有当策略中的 `network_access` 为 true
时，Codewhale 才补上 `--share-net`。智能体的 `read_only: true` shell 模式只在 Full
Access（完全访问）下开启网络（见 [子智能体](SUBAGENTS.md)）；此时它与联网的
`workspace-write` shell 一样共享主机网络命名空间，包括回环地址和抽象 unix 套接字，
bubblewrap 对两者都不做过滤。`danger-full-access` 和 `external-sandbox`
完全绕过本地包装器。

如果用户选择退出，或者 `/usr/bin/bwrap` 不存在、不可执行、或实际上无法
限制子进程，Codewhale 会报告 `none`，直接启动命令，不带任何 Linux 操作
系统包装器。这里没有回退做法：不会只打个标记，就把它当成另一种 Linux 沙箱。

在 Linux 上获得强制力，请另行安装 bubblewrap：

- Ubuntu/Debian：`apt install bubblewrap`
- Fedora：`dnf install bubblewrap`
- Arch：`pacman -S bubblewrap`

Codewhale 不自带 bubblewrap。

## Windows：不声明任何操作系统沙箱

Windows 上的命令路径目前报告未启用操作系统沙箱。源码树里有一份面向未来的辅助
程序契约，用于清理 Job Object 进程树，但它没有接入选择逻辑，也不能说成
下面任何一种能力：

- 只读文件系统或 workspace-write 的强制执行；
- 网络阻断；
- 注册表隔离；
- 受限令牌（restricted token）或 AppContainer 隔离。

Windows 主机的权限和审批策略仍然适用，但它们不是 Codewhale 的操作系统命令沙箱。

## Linux 的进程加固不等于命令沙箱

在 Linux 上启动时，Codewhale 会尽力对自己的进程设置 `PR_SET_DUMPABLE=0`、
`PR_SET_NO_NEW_PRIVS=1` 和 `RLIMIT_CORE=0`。任何一项失败都会记录日志，
启动继续进行。这些控制可以降低进程被窥探、权限被提升以及产生 core dump 的风险；
它们不为子命令建立文件系统或网络隔离，也不会被列为沙箱后端。

唯一的例外是启动姿态（posture）本身。当启动沙箱模式解析为
`danger-full-access`（通过 `CODEWHALE_SANDBOX_MODE` 或配置文件里的
`sandbox_mode` 键）时，Codewhale 会跳过 `PR_SET_NO_NEW_PRIVS`，好让
`sudo`/`su`/setuid 辅助程序能在智能体的 shell 里照常工作（#5723）——
Full Access（完全访问）指的就是这种姿态。任何更窄的启动姿态都会保留该标志作为纵深防御，
而 `CODEWHALE_NO_NEW_PRIVS` 可以双向覆盖姿态（#5413）：假值一律跳过该标志，
真值一律设置它。该标志对整个进程树都不可逆，所以只能在启动时决定；会话内单次调用升级沙箱，
也无法解除它。

## 外部 OpenSandbox 执行

配置 `sandbox_backend = "opensandbox"` 后，shell 执行会发往配置好的、兼容
OpenSandbox 的 HTTP 端点，而不是在本地启动子进程。Codewhale 会校验请求与响应
的契约，但隔离保证归所配置的服务及其运维方所有。

```toml
sandbox_backend = "opensandbox"
sandbox_url = "http://localhost:8080"
sandbox_api_key = "YOUR_API_KEY"
```

`sandbox_backend = "none"`（或省略该键）会保持本地执行。不受支持的后端设置
会拒绝 shell 执行，绝不会悄悄改用本地执行。请选择受支持的后端，或显式指定
`none`。

## 策略与回退

本地 `sandbox_mode` 的取值有：

```toml
sandbox_mode = "workspace-write" # read-only | workspace-write | danger-full-access | external-sandbox
```

- 只有当对应的包装器被选中且可用时，`read-only` 和 `workspace-write` 才由
  Seatbelt 或 bubblewrap 强制执行。
- 配置值只能收窄权限模式选定的沙箱：Full Access（`yolo`）不套包装器，Ask 和
  Auto-Review 使用 `workspace-write`，Plan 使用 `read-only`。因此配置的
  `danger-full-access` 或 `external-sandbox` 只在 Full Access 下生效，绝不会放宽
  Ask 或 Auto-Review。
- `danger-full-access` 有意绕过本地操作系统包装器。在 Linux 上，它还会在启动时
  跳过 `PR_SET_NO_NEW_PRIVS` 进程加固标志，让 `sudo`/setuid 工作流继续可用
  （#5723）；见上面的进程加固一节。
- `external-sandbox` 表示执行已经在外部隔离，因此不再套第二层本地包装器。
- 没有选中任何包装器时，shell 命令运行时就没有 Codewhale 的操作系统隔离。
  审批规则和感知工作区的原生文件工具仍是彼此独立的控制手段。

`sandbox_mode` 和外部后端都有规范的环境变量覆盖方式：

- `CODEWHALE_SANDBOX_MODE`
- `CODEWHALE_SANDBOX_BACKEND`
- `CODEWHALE_SANDBOX_URL`
- `CODEWHALE_SANDBOX_API_KEY`

`CODEWHALE_PREFER_BWRAP`（旧别名 `DEEPSEEK_PREFER_BWRAP`）可显式覆盖此偏好；
顶层 `prefer_bwrap` 配置项是持久设置。

## 诊断与失败归因

`codewhale setup --status`、`codewhale doctor`、`codewhale doctor --json` 以及
`diagnostics` 工具，都会先应用解析后的 bubblewrap 偏好，再报告本地可用的
包装器。只要某条命令的策略不要求沙箱，它仍然可以绕过这个包装器。在 Linux 上，
仅仅找到某个与沙箱相关的系统调用或源码模块，并不会让 `sandbox_available`
变成 true。

拒绝归因刻意做得很保守：

- Seatbelt 用的是它自己那套包装器专属的拒绝模式。
- Bubblewrap 的设置错误必须以 `bwrap:` 开头；bwrap 文件系统视图报出的只读
  文件系统错误，也能标识出这条边界。
- 子命令报出的通用 `Permission denied` 或 `Operation not permitted`，
  本身不能证明是 Codewhale 的沙箱挡下了它。
- 没在沙箱中运行的命令一旦失败，永远不会被标成沙箱拒绝。

## 局限

- 可用性在启动前检查；选中的包装器仍可能因为主机策略、容器限制，或探测之后
  出现的竞态而失败。
- 如果配置的可写根不存在、不是目录，或规范化之后等于 `/`，bubblewrap 会忽略它；
  路径也可能在策略解析与包装器启动之间消失。
- Seatbelt profile 在运行时生成，必须拿它要支持的那些命令实测过。
- Windows 上目前没有任何本地包装器对外声明。
- 外部沙箱后端的安全性，只取决于它所配置的服务。
- 没有任何沙箱能防住内核漏洞，也不能覆盖所有资源耗尽攻击与侧信道攻击。

## 实现参考

- `crates/sandbox/src/lib.rs` — 如实报告的选择逻辑与公开能力标记
- `crates/sandbox/src/seatbelt.rs` — macOS 包装器与可用性探测
- `crates/sandbox/src/bwrap.rs` — Linux 主动启用式包装器
- `crates/sandbox/src/process_hardening.rs` — Linux 父进程加固
- `crates/sandbox/src/backend.rs` — 外部后端选择
- `crates/tui/src/tools/diagnostics.rs` — 机器可读的诊断信息
