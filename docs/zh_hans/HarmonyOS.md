# HarmonyOS 与 OpenHarmony

> 英文原文：[HarmonyOS.md](../HarmonyOS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-11-02。

Codewhale 在鸿蒙 PC 上有两条路：在 Linux 子系统（融合开发引擎）里跑普通的 Linux
ARM64 版本，或者为 OpenHarmony 用户态原生构建一份 ARM64 程序。原生鸿蒙仍是 Tier 2
交叉构建目标；SDK 检查不能证明设备上的 TUI、编辑器或模型回合已通过验收。

## 选哪一种

| 目标 | 怎么跑 | 分发方式 |
| --- | --- | --- |
| 鸿蒙 PC 的 Linux 子系统（openEuler） | 普通的 Linux ARM64 版本 —— 它本身就是一台 Linux 虚拟机 | GitHub 发布资源；`install.sh`；npm 次要 |
| 原生 HarmonyOS 与 OpenHarmony | `aarch64-unknown-linux-ohos`，签名后才能执行 | 发布清单里有 `codewhale-ohos-arm64.tar.gz` 时直接下载；否则从源码构建并签名 |

原生鸿蒙不是 Linux 发行版：用户态是 OpenHarmony 的 musl 系 C 库和系统 API，Linux
版本在那里跑不起来。想省事就走子系统，不想在 Codewhale 和设备之间隔一层虚拟机就选
原生构建。

## 在 Linux 子系统里运行

在子系统里打开终端，用官方安装器：

```bash
curl -fsSL https://codewhale.net/install.sh | sh
"$HOME/.local/bin/codewhale" --version
```

`codewhale update` 负责升级。目录被占用、或由包管理器安装的情况，见
[迁移到全新目录](INSTALL.md#从-npmcargo-或其他安装迁移)。

华为的[支持页面](https://consumer.huawei.com/cn/support/content/zh-cn16091898/)
（中文；HarmonyOS 6.0、6.1、7.0 的台式机与笔记本；2026-10-21 查阅）列出了子系统的
限制：

- 只支持 openEuler，不能换成别的发行版。
- 没有服务管理器：`sshd` 之类要手工起。
- 内核固定，`modinfo`、`modprobe`、`rmmod`、`insmod` 都不可用。
- 不能挂载磁盘镜像。文件走 `/mnt/linux_share` 交换，那里新建的文件属主是 root ——
  `git clone` 被拒就用 `sudo git clone`。
- 不支持外接 USB 设备，不支持 IPv6；`chsh` 改不了登录 shell。
- 最多保存五个快照；只有初始用户能安装子系统。
- Docker 需要 HarmonyOS 7.0 及以上。

## 原生构建

把 `OHOS_NATIVE_SDK` 指向 OpenHarmony 原生 SDK —— 含 `llvm/bin`、`sysroot` 和
`build/cmake/ohos.toolchain.cmake` 的那个目录。

| 需要 | 说明 |
| --- | --- |
| OpenHarmony 原生 SDK（本文用 6.1 / API 26） | DevEco Studio 自带一份，在 `<DevEco>/Contents/sdk/default/openharmony/native`；公开 SDK 下载也可以 |
| 带 OHOS 目标的 Rust | `rustup target add aarch64-unknown-linux-ohos` |
| C 工具链与 `cmake` | 都来自 SDK；`scripts/ohos-env.sh` 会把它们加进 `PATH` |

Linux 或 macOS：

```bash
export OHOS_NATIVE_SDK=/path/to/openharmony/native
. ./scripts/ohos-env.sh
rustup target add aarch64-unknown-linux-ohos
export CODEWHALE_BUILD_SHA="$(git rev-parse HEAD)"
cargo build --locked --release --target aarch64-unknown-linux-ohos -p codewhale-cli
```

Windows PowerShell：

```powershell
$env:OHOS_NATIVE_SDK="<path-to-openharmony-native-sdk>"
. .\scripts\ohos-env.ps1
rustup target add aarch64-unknown-linux-ohos
$env:CODEWHALE_BUILD_SHA = (git rev-parse HEAD).Trim()
cargo build --locked --release --target aarch64-unknown-linux-ohos -p codewhale-cli
```

`ohos-env.sh` / `ohos-env.ps1` 导出该目标的链接器、`CC`/`CXX`/`AR`、CMake 工具链和
bindgen 设置，并把 SDK 自带的 `cmake` 放进 `PATH`，所以仓库里不存任何机器相关的路径。
Windows 上的最终链接走 `scripts/ohos/ohos-clang.cmd`，即使 SDK 路径带空格，
`-target aarch64-linux-ohos`、sysroot 和 `-D__MUSL__` 也会完整带到链接命令里。

产物在 `target/aarch64-unknown-linux-ohos/release/codewhale`（没有 `.exe`，在 Windows
上构建也一样）；发布流水线用的 `--profile dist` 写到
`target/aarch64-unknown-linux-ohos/dist/`。Apple 芯片笔记本上一次冷构建约 8 分钟。在意
提交标识的话请从干净检出构建 —— 增量重建不会给没被重编的产物重新打戳。先验一下产物：

```bash
file target/aarch64-unknown-linux-ohos/release/codewhale
# ELF 64-bit LSB pie executable, ARM aarch64, … dynamically linked, stripped

"$OHOS_NATIVE_SDK/llvm/bin/llvm-readelf" -d \
  target/aarch64-unknown-linux-ohos/release/codewhale | grep NEEDED
# libtime_service_ndk.so 和 libc.so —— 设备提供的两个系统库
```

## 签名与安装

原生鸿蒙设备只执行带代码签名的程序：未签名的 ELF 在 `execve` 处就被拒绝，报
`Permission denied`，`chmod +x` 没用。所以签名是构建的一部分，不是可选的加固步骤。
模拟器镜像比零售设备宽松，所以请在真正要用的设备上签名并验证。

一条命令做完：构建、拷一份 strip、自签名、验签：

```bash
scripts/ohos-sign-binary.sh
```

产物在 `target/aarch64-unknown-linux-ohos/release/codewhale-signed`（可用 `--out PATH`
另指位置）。签不上就报错退出，不会把没签好的文件交给你。手工做的话：

```bash
tools="$(dirname "$OHOS_NATIVE_SDK")/toolchains/lib"
java -jar "$tools/binary-sign-tool.jar" sign -selfSign 1 \
  -inFile target/aarch64-unknown-linux-ohos/release/codewhale \
  -outFile target/aarch64-unknown-linux-ohos/release/codewhale
java -jar "$tools/binary-sign-tool.jar" display-sign \
  -inFile target/aarch64-unknown-linux-ohos/release/codewhale   # … self-sign
```

同一个工具的 C++ 版本（`<sdk>/toolchains/lib/binary-sign-tool`，不需要 Java）参数完全
一致；从应用市场装 DevBox 还能直接在设备上拿到这个命令。想在链接期签名，就在
`.cargo/config.toml` 的 `[target.aarch64-unknown-linux-ohos]` 下加
`rustflags = ["-Clink-arg=-Wl,--code-sign"]`；SDK 的 `lld` 实现了这个开关，默认不开。

本地使用自签名就够了：它不绑定设备，能在任意一台鸿蒙 PC 上运行，但不授予任何权限。
不要用 DevEco Studio 给应用签名时自动生成的证书 —— 用它签的二进制运行时会报
`Operation not permitted`（errno 129）。需要权限就在 AppGallery Connect 申请调试证书；
Codewhale 不需要权限，也不需要 JIT。

把签好的文件拷到设备，先让它带上执行位再运行。`hdc file send` 落盘是 `0660`，而用户
目录不认 `chmod`，所以要换个方式给它 mode：在设备上从一个本来就带执行位的文件拷过去，
或者把内容覆盖已有的可执行文件，再用 `ls -l` 确认。

从应用程序自己的终端运行 —— 系统终端应用（HiShell，包名 `com.huawei.hmos.hishell`）
或 CodeArts 的集成终端 —— 不要用 `hdc shell`，那是调试通道，执行不了写在
`/data/local/tmp` 下的文件。首次启动会弹一次授权。

```bash
"$HOME/codewhale" --version
# codewhale 0.10.2 (859dc2ac7ac0)
```

## 在 CodeArts 中使用 VS Code 扩展

[CodeWhale 的 VS Code 扩展](https://github.com/HengQuWorld/CodeWhale-VSCode)
通过 HTTP webview 与引擎通信。以下介绍 CodeArts Agent 的集成路径；编辑器工作流仍需
使用确切的签名构建在设备上验收。

1. 把签好名的引擎放进 CodeArts 集成终端能看到的 `PATH`，或者用
   `brotherwhale.enginePath` 指到文件。
2. 在扩展面板里打开 Open VSX 页，安装 "CodeWhale"（发布者 `hengquworld`）。CodeArts
   读的就是这个插件源，不必手动装 VSIX。
3. 打开一个工作区目录，开始对话。扩展会按需拉起引擎，并在状态栏显示 Ready。

## 设备上能做什么

既有观察来自一台 HarmonyOS 7.0.0.110 / API 26 的 2in1 设备上的自签名构建；
不能据此认定后续构建或下表中尚未验证的工作流已通过验收。

| 能力 | 现状 |
| --- | --- |
| `--version`、`--help`、`doctor` | 已观察到可用 |
| `auth set` / `auth status` | 本地命令可启动；选定状态目录上的完整写入和读取需用下述 store smoke 验证 |
| CodeArts Agent 里的 VS Code 扩展 | 上述集成路径；确切构建的设备工作流尚未验证 |
| `exec_shell` | 设备提供 `/bin/sh`；子进程启动仅经探针观察，未通过模型回合验证 |
| 交互式 TUI | 待设备验收；请用 HiShell 或 CodeArts 集成终端，`hdc shell` 不分配 tty |
| 本地沙箱 | 不存在：命令 best-effort 执行，引擎会说明 |
| 常驻 PTY 工具（`terminal/*`） | 在该目标上不注册 |
| 剪贴板 | OSC 52（tmux 内 `load-buffer -w`）；只有文本，无图片粘贴 |
| `codewhale update` | 被编译排除；换新时把新的签名二进制放到位即可 |
| 一切需要 `git` 的能力 | 你提供 `git` 才有；快照和 `/undo` 会说明原因 |

原生鸿蒙终端不是 Linux shell：没有 `/bin/bash`，没有包管理器。缺什么 Codewhale 就报
什么，不会拿别的东西顶上。

## 用户目录

`/storage/Users/currentUser` 是 hmdfs，一种分布式文件系统：它把登录用户报成所有对象的
属主，并用自己的 mode（文件 `0660`、目录 `2771`）代替请求的 mode。Codewhale 的状态和
密钥就像在其他平台一样存在这里；store 会打一条警告说明它看到的 mode —— 保护这些文件的
是这个挂载自身的访问控制。

有得选的时候：

- **要严格权限检查**就得用会保存属主与 mode 的文件系统。把 `CODEWHALE_HOME` 指到应用
  沙箱即可，路径要短：daemon 的 socket 是 `<CODEWHALE_HOME>/run/daemon.sock`，内核拒绝
  超过 107 字节的路径。

  ```sh
  CODEWHALE_HOME=/data/storage/el2/base/files/cw codewhale serve --http
  ```

  `CODEWHALE_HOME` 会整体搬走状态根目录，配置和会话也在内。
- **密钥**也可以走 provider 的环境变量，代价是它在 shell 配置里是明文：

  ```bash
  export DEEPSEEK_API_KEY=sk-…
  codewhale
  ```

- **一个工作区只有一个驱动者。** 引擎按工作树获取租约，第二个会被拒绝，并报出持有者的
  pid、客户端和时长。终端和编辑器打开同一个目录也算：先关掉一个再开另一个。
  若文件系统拒绝咨询锁，引擎会明确报告无法提供独占保证；该挂载不能排除两个驱动者。

`scripts/ohos-secrets-store-smoke.sh <目录>` 对着真实目录端到端验一遍 store：写入 key、
用 `auth status` 读回来，并确认 `config.toml` 里没有明文副本。

## 常见故障排查

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `aws-lc-sys` 报 `Required build dependency is missing. Halting build.` | `PATH` 里没有 `cmake` | 重新 `. ./scripts/ohos-env.sh`（它加上 SDK 的 `cmake`），或自行安装 `cmake` |
| 运行二进制报 `Permission denied` | 未签名，或没有执行位，或从 `hdc shell` 启动 | 签名、连同执行位一起拷进去，从 HiShell 或 CodeArts 里运行 |
| 签名后仍报 `Operation not permitted`（errno 129） | 用了 DevEco Studio 自动签名生成的应用证书 | 改用 SDK 的自签名，或用 AppGallery Connect 的调试证书 |

## 工具链备注

- **编译器包装脚本。** `scripts/ohos/ohos-clang.{sh,ps1}` 与
  `ohos-clangxx.{sh,ps1}` 读 `OHOS_NATIVE_SDK`，不含任何本机路径；POSIX 版本直接执行前
  先 `chmod +x`。
- **不存链接器路径。** Cargo 不会展开 `linker` 或 CMake 工具链值里的环境变量，所以这些
  都由 `ohos-env` 导出。
- **依赖守卫。** `./scripts/release/check-ohos-deps.sh` 不需要 SDK，一旦目标扛不住的
  crate（`portable-pty`、`nix`、`starlark`、`arboard`、`keyring`）重新进入依赖图就失败。
  真正的构建在 `.github/workflows/ohos.yml` 里。
- **确认构建版本。** 上述命令提供 `CODEWHALE_BUILD_SHA`，将其前 12 位写入
  `--version`。未提供标识的本地构建显示 `(dev)`；仅检出 `main` 不会自动写入提交号。
  本地重新构建不会替换已安装的二进制。
