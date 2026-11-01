# HarmonyOS 与 OpenHarmony

> 英文原文：[HarmonyOS.md](../HarmonyOS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-10-30。

本文讲 Codewhale 在 HarmonyOS PC 上怎么跑、OpenHarmony 的交叉编译怎么做，
以及设备允许产物做什么、不允许做什么。

## 支持层级

| 目标 | Codewhale 层级 | 验证方式 | 分发方式 |
| --- | --- | --- | --- |
| HarmonyOS PC 的 Linux 子系统（融合开发引擎，openEuler） | Tier 1 Linux ARM64 运行时 | 由 Linux ARM64 发布构建覆盖；没有在 Linux 子系统内运行的检查 | GitHub 发布的二进制文件；npm 为次要途径 |
| 原生 HarmonyOS 与 OpenHarmony（`aarch64-unknown-linux-ohos`） | Tier 2 交叉编译目标 | 手动触发 SDK 检查，覆盖 `codewhale-tui` 和 `codewhale-cli`；发布构建还会校验它产出的自签名 | 发布中列出时为 `codewhale-ohos-arm64.tar.gz`，否则从源码构建 |

Linux 子系统是运行 openEuler Linux 的虚拟机，因此使用 Linux ARM64 发布版。
原生 HarmonyOS 和 OpenHarmony 不是 Linux 发行版：它们的用户空间使用 OpenHarmony
基于 musl 的 C 库和系统接口，Codewhale 为它构建原生版本时使用
`aarch64-unknown-linux-ohos` 目标。Linux 发布二进制文件没有针对原生 HarmonyOS
构建或测试。

Tier 2 承诺的是：可用的源码构建路径、手动触发的 SDK 检查，以及在发布中附带签名产物。
它不承诺每个提交都跑检查，也不承诺 CI 内的设备级运行测试。OpenHarmony 工作流使用
已发布的 OpenHarmony 6.1 原生 SDK；如果 SDK、Clang 或 sysroot 缺失，它会**故意失败**，
而不是拿宿主头文件或桩件顶替、报出一个假成功。

## 在 HarmonyOS PC 上运行

在 HarmonyOS PC 上，最省事的路径是 Linux 子系统（融合开发引擎）。华为将它
描述为“轻量级虚拟化开发环境（Linux子系统）”，当前版本仅支持 openEuler。
全新安装时，在 Linux 子系统里打开终端，用官方 GitHub 安装脚本：

```bash
curl -fsSL https://codewhale.net/install.sh | sh
"$HOME/.local/bin/codewhale" --version
```

[已发布的 v0.9.11 版本](https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.9.11)
包含 `codewhale-linux-arm64` 和 `codew-linux-arm64`。各版本的具体要求和 Cargo 兜底方案见
[Linux ARM64 可移植性](./INSTALL.md#linux-arm64-可移植性)。
已经直接安装过的，用 `codewhale update`。目录已被占用，或者由包管理器管理的安装，
见[迁移到全新目录](./INSTALL.md#migrating-from-npm-cargo-or-another-installation)。

华为的 [Linux 子系统支持页面](https://consumer.huawei.com/cn/support/content/zh-cn16091898/)
（适用于 HarmonyOS 6.0、6.1 和 7.0 的台式机与笔记本；2026-10-21 核对）列出了以下限制：

- 当前版本融合开发引擎仅支持 openEuler。
- 不支持 `systemctl` 等服务管理工具；需要手动拉起服务，例如
  `sudo /usr/sbin/sshd`。
- 不支持对内核进行修改，`modinfo`、`modprobe`、`rmmod` 和 `insmod` 会报错。
- 不支持用 `mount` 命令直接挂载镜像。可以通过共享文件夹交换文件，它在 Linux
  里的路径是 `/mnt/linux_share`。共享文件夹内创建的文件为 root 属主，默认用户下
  `git clone` 可能失败；华为给出的替代做法是 `sudo git clone`。
- 暂不支持外接 USB 设备和 IPv6，也不支持用 `chsh` 修改登录默认 Shell。
- 最多支持保存 5 个快照。
- 仅支持在主用户（即电脑首次开机创建的用户）下安装使用。华为商用鸿蒙电脑系统版本为
  HarmonyOS 6.0.0.130 及以上时，也支持在个人空间安装使用。
- Docker 需要 HarmonyOS 7.0 及以上版本。

## 原生目标只运行签过名的代码

原生 HarmonyOS/OpenHarmony 设备只执行带合法代码签名的程序。未签名的 ELF 在
`execve` 处就会被拒绝，报 `Permission denied`——无论权限位怎么写，`chmod +x` 也没用。
这是平台的强制代码签名机制，不是 Codewhale 的问题，`sh`、`toybox` 和任何第三方
二进制都一视同仁。

模拟器镜像比零售设备宽松：它允许从 `hdc shell` 执行拷到 `/data/local/tmp` 的未签名
二进制。因此在模拟器上能跑的构建，到了真机仍可能被拒绝。签名和验证请在目标设备上做。

由此有两条结论，后面所有内容都受它影响：

1. **产物必须签名。** 本地使用 `binary-sign-tool … -selfSign 1` 就够了：它证明文件
   没有被改动，签出来的文件可以拷到任意一台鸿蒙电脑上运行（自签名不绑定设备）。
   但它不授予任何权限——要权限得用绑定设备 UDID 的调试证书，或者华为签发的二进制证书。
2. **`hdc shell` 不是运行它的地方。** 那个 shell 是调试通道，有自己的安全标签
   （`u:r:sh:s0`），不能执行写在 `/data/local/tmp` 下的文件（这些文件带
   `u:object_r:data_local_tmp:s0`）。一个**签名完全正确**的二进制在那里照样报
   `Permission denied`，哪怕文件是 `-rwxr-xr-x`。要从系统自带终端应用（HiShell，
   包名 `com.huawei.hmos.hishell`）里运行；首次启动会弹一次授权。

## 交叉编译到 OpenHarmony

仓库不把机器相关的 SDK 路径纳入版本控制。把 `OHOS_NATIVE_SDK` 设成 OpenHarmony
原生 SDK 目录，也就是包含 `llvm/bin`、`sysroot` 和
`build/cmake/ohos.toolchain.cmake` 的那个目录。

前置条件：

| 依赖 | 说明 |
| --- | --- |
| OpenHarmony 原生 SDK（本文用 6.1 / API 26） | DevEco Studio 自带一份，位于 `<DevEco>/Contents/sdk/default/openharmony/native`；公开 SDK 下载包和 CI 用的 `setup-ohos-sdk` action 也可以 |
| 带 OHOS 目标的 Rust | `rustup target add aarch64-unknown-linux-ohos`——Tier 2 目标，标准库有预编译件 |
| `cmake` 与构建工具 | SDK 自带在 `build-tools/cmake/bin`；`scripts/ohos-env.sh` 会把它们加进 `PATH` |

Windows PowerShell：

```powershell
$env:OHOS_NATIVE_SDK="<path-to-openharmony-native-sdk>"
. .\scripts\ohos-env.ps1
rustup target add aarch64-unknown-linux-ohos
$env:CODEWHALE_BUILD_SHA = (git rev-parse HEAD).Trim()
cargo build --locked --release --target aarch64-unknown-linux-ohos -p codewhale-cli
```

Linux 或 macOS：

```bash
export OHOS_NATIVE_SDK=/path/to/openharmony/native
. ./scripts/ohos-env.sh
rustup target add aarch64-unknown-linux-ohos
export CODEWHALE_BUILD_SHA="$(git rev-parse HEAD)"
cargo build --locked --release --target aarch64-unknown-linux-ohos -p codewhale-cli
```

默认可执行文件是 `target/aarch64-unknown-linux-ohos/release/codewhale`，在 Windows
上构建也没有 `.exe` 后缀。如果设置了 `CARGO_TARGET_DIR`，请使用对应的输出目录。
在 Apple 芯片笔记本上，冷启动的 release 构建大约需要 8 分钟。要构建发布用的档位，
把 `--release` 换成 `--profile dist`——发布工作流用的就是这个档位（fat LTO）。

使用上述提交标识时，应从没有未提交改动的源码构建。`0.10.2 (859dc2ac7ac0)` 这样的
版本串来自 `CODEWHALE_BUILD_SHA`，而增量构建不会给一个无需重编的产物重新打标。

复制到任何地方之前，先核对产物：

```bash
file target/aarch64-unknown-linux-ohos/release/codewhale
# ELF 64-bit LSB pie executable, ARM aarch64, … dynamically linked, stripped

"$OHOS_NATIVE_SDK/llvm/bin/llvm-readelf" -d \
  target/aarch64-unknown-linux-ohos/release/codewhale | grep NEEDED
# libtime_service_ndk.so 和 libc.so —— 设备提供的两个系统库
```

这些环境准备脚本会为 `aarch64-unknown-linux-ohos` 导出 Cargo 的目标专用
`linker`、`AR`、`CC`、`CXX`、`CFLAGS`、`CXXFLAGS`、`CARGO_ENCODED_RUSTFLAGS`、
`CC_SHELL_ESCAPED_FLAGS`，以及 CMake 工具链变量。它们还会把 `bindgen` 指向 SDK 的
`libclang` 和 sysroot，好让 `rquickjs-sys` 生成 OpenHarmony 绑定；
`rquickjs-sys` 本身并不附带这些绑定的预生成版本。脚本还会把 SDK 自带的 `cmake`
放进 `PATH`，这样带 CMake 构建脚本的 crate（TLS 栈依赖的 `aws-lc-sys`）就不必
指望宿主机刚好装了 cmake。

在 Windows 上，`ohos-env.ps1` 让 Cargo 使用仓库里的 `ohos-clang.cmd` 启动器。
该启动器再委托给 `ohos-clang.ps1`，所以不只 C/C++ 编译和
bindgen，最终那次 Rust 链接也总会带上 `-target aarch64-linux-ohos`、
SDK sysroot 和 `-D__MUSL__`，同时保留 Cargo 的链接器参数和退出状态。启动器转发前会重新给每个参数加引号，
所以就算 SDK 路径里带空格（比如默认的 `D:\DevEco Studio\...` 安装），
到最终链接时 `--sysroot` 依然完好。

## 给构建产物签名

在设备上运行之前，用 SDK 里的 `binary-sign-tool` 给它签名：

```bash
SDK_TOOLCHAINS="$(dirname "$OHOS_NATIVE_SDK")/toolchains/lib"
java -jar "$SDK_TOOLCHAINS/binary-sign-tool.jar" sign \
  -inFile target/aarch64-unknown-linux-ohos/release/codewhale \
  -outFile target/aarch64-unknown-linux-ohos/release/codewhale \
  -selfSign 1
```

同一个工具的 C++ 版本（`<sdk>/toolchains/lib/binary-sign-tool`，不需要 Java）参数完全一致；
鸿蒙 PC 用户也可以从应用市场装 DevBox，直接在设备上拿到这个命令。确认签名确实写进去了：

```bash
java -jar "$SDK_TOOLCHAINS/binary-sign-tool.jar" display-sign \
  -inFile target/aarch64-unknown-linux-ohos/release/codewhale
# … code signature is self-sign
```

如果更想在链接期完成签名，可以加链接器参数——SDK 的 `lld` 实现了 `--code-sign`，
但默认不开启：

```toml
# .cargo/config.toml
[target.aarch64-unknown-linux-ohos]
rustflags = ["-Clink-arg=-Wl,--code-sign"]
```

两个必须知道的限制：

- **自签名不授予权限。** 不管 `module.json` 里写了什么，`display-sign` 都会报
  `permission is not found`。不要复用 DevEco Studio 给应用签名时自动生成的那张证书：
  用它签的二进制在运行期会被拒绝，报 `Operation not permitted`（errno 129）。
  正确做法是在 AppGallery Connect 申请调试证书再签。
- **自签名不授予 JIT。** 任何在运行期映射可执行页的东西（模拟器、带 JIT 的 JS 引擎）
  都得打成 public HNP 塞进 HAP。Codewhale 不需要。

把签好的文件拷到设备，从系统终端里运行：

```bash
"$HOME/codewhale" --version
# codewhale 0.10.2 (859dc2ac7ac0)
```

## 设备上能做什么、不能做什么

表里区分两件事：在设备上实际观察到的，和目标侧 `cfg` 开关让哪些能力从一开始就不可达。
观察结果来自一台 HarmonyOS 7.0.0.110 / API 26 的 2in1 设备，用自签名构建测得，
只能当作起点，不是兼容性矩阵。

| 能力 | 现状 | 依据 |
| --- | --- | --- |
| `--version`、`--help`、`doctor` | 可用 | 设备实测 |
| 只读本地状态的子命令（`auth status`、`auth set` 的输入提示） | 能跑 | 设备实测 |
| `exec_shell`（经非 PTY 的进程实现执行 `/bin/sh -c …`） | 取决于设备是否提供 `/bin/sh`，本设备提供；用同一套 API 派生进程的探针测试成功 | 探针实测，尚未经模型回合验证 |
| 交互式 TUI | 未在设备上验证 | `hdc shell` 不分配 tty，尚未有一次从 HiShell 画出 TUI 的运行 |
| 本地沙箱 | 不存在（`sandbox not available (commands run best-effort)`） | 代码：`all(target_os = "linux", not(target_env = "ohos"))` |
| 常驻 PTY 工具（`terminal/*`） | 不注册 | 代码：`portable-pty` 被排除在依赖图外 |
| 剪贴板 | OSC 52（tmux 内用 `load-buffer -w`）；无图片粘贴 | 代码 |
| `codewhale update` 与自更新 | 被编译排除 | 代码 |
| 一切需要 `git` 的能力 | 用户不提供就没有；快照和 `/undo` 会说明原因 | 代码，外加一次设备上的告警 |

原生鸿蒙终端不是 Linux shell：没有 `/bin/bash`，没有 `git`，没有包管理器。
Codewhale 会降级而不是假装——缺什么就报什么，不会静默替换。

### 用户目录里的凭据存储

2in1 的用户目录（`/storage/Users/currentUser`）是分布式文件系统（hmdfs），
行为和本地文件系统不一样：

- 每个对象都被报成登录用户所有（实测设备上是固定值 `20001006`），与创建它的 uid
  无关；
- 新文件按固定基线 `0660` 落盘、新目录按 `2771`，而不是请求的 mode；
- 正因为调用者从来不是被报告的那个属主，之后对这类对象执行 `chmod` 会得到 `EPERM`
  ——不管 mode 写着什么，它都不是调用者的文件。

文件版 secret store 需要“仅本人可读写”的文件和跨进程锁，所以 `codewhale auth set`
在那里曾失败，报 `insecure permissions`。现在的 store 用证据判断，而不是只看 mode：
它请文件系统收紧一个探针文件、再把 mode 读回来，只有那样才认定 mode 说明不了问题。
把 `CODEWHALE_HOME` 指到 `/data`（hmfs，正常保存属主与 mode）下的目录可以恢复严格
校验；也可以用 provider 的环境变量提供 key——当 store 无法回答时，会读这个环境变量：

```bash
export DEEPSEEK_API_KEY=sk-…
codewhale
```

环境变量在 shell 配置里是明文，请知情后再选。在 shell 里读回来的 mode 并不能回答
store 能否在那里工作——在这个挂载上，文件被报成 `0640`，而本该收紧它的 `chmod` 会被
拒绝，因为 shell 并不是被报告的那个属主。store 的做法是去问文件系统，然后说明它得到
了哪个答案：直接运行 `codewhale auth set`，读它为 store 路径记下的那条警告。

## 常见故障排查

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `aws-lc-sys` 报 `Required build dependency is missing. Halting build.` | `PATH` 里没有 `cmake` | 重新 `. ./scripts/ohos-env.sh`（它会加上 SDK 的 `cmake`），或者自行安装 `cmake` |
| 检查 `codewhale-cli` 时报 `function sha256_hex is never used` | 只有更新器使用的模块被编译进了没有更新器的目标 | 代码里已修：让 `mod hashing` 与 `mod update` 用同一个门控；旧检出需要给两者都加上 `#[cfg(not(target_env = "ohos"))]` |
| 从 `hdc shell` 运行产物报 `Permission denied` | 调试 shell 不能执行 `/data/local/tmp` 下的文件；而且只有签名代码才能运行 | 先签名，再从 HiShell 运行 |
| 签名后仍报 `Operation not permitted`（errno 129） | 用了 DevEco Studio 自动签名生成的应用证书 | 在 AppGallery Connect 申请调试证书，用它来签 |
| `Secret storage write failed … insecure permissions` | 该挂载存不住 `0600` | 见上文「用户目录里的凭据存储」 |
| 登录报 `opening filesystem root` | 准备凭据目录阶段，早于浏览器登录 | 报告时附上 `--version`、完整构建提交、设备/系统与终端版本；不要为了绕过而放开系统目录 |

## 确认开发构建的版本

当前开发代码在 `main`。`0.10.2 (dev)` 表示构建时没有提供源码提交标识，并不是一个
开发通道，也不代表二进制包含了最新修复。上面的命令会把源码提交号的前 12 位写进
`--version`。请在二进制旁边一并记录完整的 `git rev-parse HEAD`。本地重新构建不会
自动替换已经装好的旧二进制；请检查你实际复制的那一个可执行文件。

GitHub 上仅有构建产物的 release candidate artifact 会标明源码提交，但它不代表具备
设备级支持。

## 登录时的文件系统错误

`ChatGPT sign-in failed: opening filesystem root` 出现在准备本地凭据存储阶段，
早于浏览器登录或回调设置。它本身不足以说明 `/tmp` 是只读的。当前源码对
Linux/OpenHarmony 的上级目录只用遍历句柄，对私有凭据目录保留一个正常的、受保护的
句柄。打开根目录失败时会带上系统错误码。

报告问题时，请附上确切的 `--version`、完整构建提交、设备/系统与终端版本，以及完整
错误文本。不要在报告里放 token、授权 URL 或凭据文件。不要把系统目录改成可写，
也不要关闭凭据检查。构建成功或在 Linux 上复现权限问题，仍然需要在受影响的设备上
做一次登录验证。

## 用户目录上的工作区独占

驱动中的 Engine 会给每个工作树取一份租约，这样第二个驱动者会被拒绝，而不是两个
Engine 同时写同一棵树、同一份 snapshot 侧仓和同一个 git index。租约放在状态根下：
`$CODEWHALE_HOME/workspace-leases`，否则 `~/.codewhale/workspace-leases`；在 2in1 上它
位于 hmdfs。

hmdfs 证明不了那个目录属于当前用户：这个区域上的每个对象都被报成登录用户所有，与
创建它的 uid 无关，因此调用进程从来不是被报告的那个属主。受限的是「证明」，不是
「锁」——这是两种能力。租约以「属主校验无法验证」的状态准入并照常获取，所以互斥依然
成立：同一进程内的两个引擎作用域共享同一个槽位，另一个应用打开同一工作区时仍会带着
持有者的 pid、客户端、store 和持有时长被拒绝。

只有当文件系统拒绝咨询锁本身时（`flock`/`fcntl` 返回 `EPERM`、`EACCES`、`ENOTSUP`、
`EINVAL` 或 `ENOSYS`，部分网络与 FUSE 挂载如此），Engine 才会在没有独占的情况下运行，
并在每个准入的工作区作用域报一次。

更早的构建会把同一种情况变成启动失败：

```
Engine exited before becoming ready: error: workspace drive lease check failed
for /storage/Users/currentUser/<workspace>: Codewhale credentials directory must
be owned by the current user
```

这句话两半都没说到点上：既没说真正的目录（`workspace-leases` 状态根），也没说真正
的原因（hmdfs 合成属主）。其中「credentials」的措辞继承自私有目录机制，那套机制最
初来自 xAI OAuth 存储。

如果要在 HarmonyOS PC 上得到真正的跨客户端保证，请把 Engine 的私有状态放到应用
沙箱，它的文件系统正常保存属主和 mode：

```sh
CODEWHALE_HOME=/data/storage/el2/base/files/cw codewhale serve --http
```

`CODEWHALE_HOME` 会整体搬走状态根，包括配置和会话，请先确认这是你要的。路径要短：
daemon socket 是 `<CODEWHALE_HOME>/run/daemon.sock`，内核拒绝超过 107 字节的 socket
路径。

不要用 `chmod` 去修这个拒绝，也不要把用户目录改成组可写：该挂载会忽略 mode，下一
次运行会得到同样的结论。

## 编译器包装脚本

临时手工调用编译器时，用 `scripts/ohos/` 里的包装脚本。它们读同一个
`OHOS_NATIVE_SDK` 变量，不包含任何本地路径。

Windows PowerShell：

```powershell
.\scripts\ohos\ohos-clang.ps1 --version
.\scripts\ohos\ohos-clangxx.ps1 --version
```

Linux 或 macOS：

```bash
sh ./scripts/ohos/ohos-clang.sh --version
sh ./scripts/ohos/ohos-clangxx.sh --version
```

如果想直接以 `./scripts/ohos/ohos-clang.sh` 形式运行 POSIX 包装脚本，先给它们加执行位：

```bash
chmod +x ./scripts/ohos/ohos-clang.sh ./scripts/ohos/ohos-clangxx.sh
```

## 链接器与工具链路径

仓库不纳入 Cargo 链接器路径或 CMake 工具链路径。Cargo 不会展开 `linker` 或 CMake
工具链路径值里的环境变量，所以这些值由 `scripts/ohos-env.ps1` 和
`scripts/ohos-env.sh` 导出。

## 依赖守卫

发布准备阶段会跑一个不需要 SDK 的依赖检查：

```bash
./scripts/release/check-ohos-deps.sh
```

这个守卫会断言 Windows 最终链接包装脚本的契约，证明 OHOS 会启用
`rquickjs-sys` 的 bindgen 特性，解析 `codewhale-tui` 在
`aarch64-unknown-linux-ohos` 上的依赖图，并在以下不受支持的主机/UI crate
重新进入依赖图时失败：`nix` 0.28/0.29、`portable-pty`、`starlark`、`arboard`、
`keyring`。这个无 SDK 检查不能替代真正的 SDK/sysroot 构建，但它能在发布前拦住已知的
链接器、bindgen、`starlark -> rustyline -> nix` 以及 PTY/keyring 回归。

因为 `portable-pty` 被有意排除在 OpenHarmony 依赖图之外，常驻的 `terminal/*` PTY
工具不会在该目标上注册。普通的 `exec_shell` 工具仍通过其非 PTY 的进程实现提供。

仅 Linux 的沙箱实现（bubblewrap 与 `prctl` 进程加固）只在
`all(target_os = "linux", not(target_env = "ohos"))` 下编译。因此 OpenHarmony 报告
没有本地 OS 沙箱，而不是去探测它并不支持的 Linux 内核路径或系统调用。配置之后，
外部 OpenSandbox 执行仍可单独使用。

原生桌面剪贴板库和 Wayland 辅助工具也被排除在 OpenHarmony 依赖图之外。文本复制
降级到终端客户端路径（OSC 52，tmux 内则用 `load-buffer -w`）；粘贴由终端以普通/
括号粘贴输入提供。该目标上无法读取图片剪贴板。如果终端无法接受 OSC 52，复制会返回
明确的 "Clipboard unavailable" 错误，而不是 panic 或谎报成功。
