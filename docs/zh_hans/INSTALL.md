# 安装 Codewhale

> 英文原文：[INSTALL.md](../INSTALL.md)。
> 最后与英文同步日期（last synced with English revision）：2026-10-06。按当前英文源码复核；下文 v0.10.0 安装记录与附录的原始测试范围保留，不能当作 v0.10.1 发布安装回执。

Codewhale 是一个在终端里运行的开源编码智能体（coding agent）。你交给它一个任务（"修复失败的测试"、"加一个 CLI 参数"），它会读取你的仓库、编辑文件并运行命令。在默认的 **Ask**（询问）权限级别下，它会立即应用工作区内的文件编辑（并给你看 diff），但运行 shell 命令之前会先询问，所以请先提交或暂存你在意的改动。它支持多家模型提供商，默认是 **DeepSeek**。

命令是 `codewhale`。`codew` 是同一个程序的较短别名。

安装路径最初是在一台全新的 **Ubuntu 24.04 x86_64** 机器上使用 **v0.10.0**（2026-09-22 发布）验证的。带日期的[安装回执](https://github.com/codewhale-hq/Codewhale/blob/37ecdfcc49bc68a9b0d058b97c3946e62c34bd31/docs/install-report/v0.10.0-2026-09-23/RECEIPTS.md)记录了当时实际运行的命令和核对的输出。在那台机器上无法运行的步骤标注为 **（该虚拟机上未测试：原因）**。macOS、Windows 和 Android 不在测试范围内，只有少量说明。第二轮在 **macOS 26.1（Apple silicon）** 上重新运行了安装器、手动下载、压缩包和 npm 路径、无密钥检查以及 zsh 补全，见 [macOS 说明](#macos-说明)。需要调用模型的步骤没有在 macOS 上重跑。

使用 `latest` 的安装命令会解析到最新**已发布**的 GitHub Release 或包。两次发布之间，`main` 可能已经在描述下一个版本（例如尚未打标签的 v0.10.1 源码候选版）。预编译候选版只有在标签、校验和与发布资源齐备后，才可通过官方安装器获取；贡献者可以先从 `main` 构建源码。

## 用一个账号管理提供商密钥

[注册](https://app.codewhale.net/register)或[登录](https://app.codewhale.net/login)
后，就能集中管理模型提供商的 API 密钥。在账号的[提供商设置](https://app.codewhale.net/providers)
中添加、更换或移除密钥，也可以使用下面的终端命令。换一台设备时，登录并选择
Codewhale 模型通道，就能使用同一组账号密钥，无需再次粘贴。

```bash
codewhale login
codewhale account keys set deepseek  # 隐藏输入，不回显密钥
codewhale account keys list         # 只显示状态，不显示密钥
codewhale --provider codewhale      # 使用账号的模型通道
```

这里的账号设置说明针对 0.10.1 源码候选版。登录不会自动上传已有的本地密钥。
如果明确希望复制本地密钥，请使用 `codewhale account keys set deepseek --from-local`。
账号密钥保存在服务端，用于经过账号通道的请求，不会下载到每台设备的本地密钥存储。
不注册账号也可以使用本地密钥或本地模型。更换密钥、选择账号或本地通道的说明见
[账号密钥设置](CONFIGURATION.md#账号提供商密钥)。

## 使用当前源码参与测试与贡献

`latest` 表示最新已发布版本；想测试尚未发布的修复，请使用权威 GitHub
`main`，或确认与它提交一致的 CNB `main`。不要把仓库版本号当作发布证明。

```bash
git clone --branch main https://github.com/codewhale-hq/CodeWhale.git
cd CodeWhale
cargo build --release --locked -p codewhale-cli
target/release/codewhale --version
```

构建前置条件见下文 Cargo 章节；使用仓库选定的 stable Rust，最低声明版本
为 1.89。首次构建可能耗时较长。Windows 的可执行文件为
`target/release/codewhale.exe`，并需要 MSVC 工具链。开发构建不等于已签名发布包。

中国大陆网络可参阅 [CNB 镜像、下载与验证](CNB_MIRROR.md)。npm 的
`--registry=https://registry.npmmirror.com` 只改变 npm 包来源；包装器仍需要
从独立的 GitHub、有效的 CNB 发布或完整的 HTTPS 资产镜像下载二进制。
腾讯轻量应用服务器自建路径见 [Lighthouse 中文/英文引导](../../scripts/tencent-lighthouse/README.md)。

---

## 60 秒快速上手（Linux 或 macOS）

```bash
# 1. Install. Downloads two checksum-verified binaries into ~/.local/bin (no sudo).
curl -fsSL https://codewhale.net/install.sh | sh

# 2. Make sure ~/.local/bin is on your PATH, now and in future terminals.
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc    # zsh: use ~/.zshrc
export PATH="$HOME/.local/bin:$PATH"
codewhale --version          # -> codewhale 0.10.0 (1be1a703b975)

# 3. Give it a DeepSeek API key (from https://platform.deepseek.com/api_keys).
codewhale auth set --provider deepseek    # prompts for the key; nothing is echoed
codewhale auth status --provider deepseek # "active source: secret store"

# 4. Run your first task inside a git repository.
cd ~/your-project
codewhale
```

在 TUI 里输入一句具体的话：

```text
create a Python file primes.py that prints the first 10 primes, run it, and show me the output
```

Codewhale 会写出文件、给你看 diff，然后询问 **APPROVAL: bash python3 primes.py – Do you want to proceed?**。按 `y` 允许本次执行。它会运行该命令并报告输出。（输入框为空时）按 `Ctrl-D` 退出，它会打印出之后恢复该会话的命令。

> **如果发送第一条消息后没有任何反应，** 说明你还没有配置密钥。v0.10.0 在这种情况下不会给出警告。按 **F3**。如果 DeepSeek 显示 `missing key`，按 Enter，粘贴密钥，然后选择模型并确认。

---

## 目录

1. [开始之前](#1-开始之前)
2. [安装：推荐的安装器](#2-推荐的安装器curl--sh)
3. [安装：从 GitHub Releases 手动下载](#3-从-github-releases-手动下载)
4. [安装：npm](#4-npm)
5. [安装：Cargo / 从源码构建](#5-cargo-与从源码构建)
6. [安装：Homebrew（Linux）与 Nix](#6-linux-上的-homebrew-与-nix)
7. [更新与回滚](#7-更新与回滚)
8. [API 密钥与提供商](#8-api-密钥与提供商)
9. [Shell 补全](#9-shell-补全)
10. [运行：TUI、无头模式、恢复会话](#10-运行)
11. [终端说明（Ghostty 及其他）](#11-终端说明)
12. [卸载，以及 Codewhale 留下了什么](#12-卸载)
13. [故障排查](#13-故障排查)
14. [附录：其他平台（本版未重测）](#附录其他平台本版未重测)

---

## 1. 开始之前

| 你需要 | 原因 |
|---|---|
| Linux x86_64 或 arm64，或 macOS | 这些平台有预编译二进制。Linux 二进制是**静态**链接的（musl），没有 glibc 或 libdbus 依赖，可在任何发行版上运行。 |
| `curl`（或 `wget`）和 `sha256sum`（或 `shasum`） | 安装器用它们下载并校验。 |
| 一个模型提供商的密钥，例如 [DeepSeek](https://platform.deepseek.com/api_keys) | 没有模型，Codewhale 什么用都没有。 |
| `git`（推荐） | Codewhale 在 git 仓库里效果最好。 |
| 可选：Python 3、Node.js 20+ | 如果存在，Codewhale 会启用它的 Python 和 JS 执行工具（`codewhale doctor` 会列出它们）。 |

**该选哪条安装路径？**

* **大多数人：** [推荐的安装器](#2-推荐的安装器curl--sh)。它最快（在测试机上约 6 秒），会校验校验和，并支持 `codewhale update`。
* **隔离网络（air-gapped）或需要安全审查的机器：** [手动下载](#3-从-github-releases-手动下载)。
* **你已经用 npm 管理命令行工具：** [npm](#4-npm)。
* **你的平台没有预编译二进制，或者想自己构建：** [Cargo](#5-cargo-与从源码构建)。

**只选一种。** 同一台机器上装了多份，最后会在 PATH 上互相打架（见[故障排查](#13-故障排查)）。

**隐私说明：** Codewhale **默认**会发送聚合的使用次数统计（PostHog）。要永久关闭：运行 `codewhale config set telemetry false`，或者导出 `CODEWHALE_TELEMETRY=0`（它始终优先）。TUI 启动时还会检查 GitHub 上是否有更新（`~/.codewhale/config.toml` 中的 `[update] check_for_updates`）。

---

<a id="recommended-official-github-releases"></a>

## 2. 推荐的安装器（`curl | sh`）

**前置条件：** curl、`sha256sum`（Linux）或 macOS 自带的 `shasum`，以及一个可写的主目录。不需要 sudo、Node 或 Rust。

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

它做了什么（已验证）：

* 检测你的平台（`linux-x64`、`linux-arm64`、`macos-x64`、`macos-arm64`）。对 Android/Termux 和 riscv64，它会给出明确提示并拒绝安装。
* 从最新的 GitHub Release 下载 `codewhale-<platform>`、`codew-<platform>` 和 `codewhale-artifacts-sha256.txt`，并对照清单校验两个二进制。只要有一个不匹配，就会在安装任何东西之前停止（`codewhale install: checksum mismatch for …`）。
* 安装 `~/.local/bin/codewhale` 和 `~/.local/bin/codew`：两个内容相同的 78 MB 文件。
* **从不使用 sudo，也从不修改你的 shell 配置文件。** 它拒绝安装到系统或包管理器目录（`/usr/bin`、`~/.cargo/bin`、Homebrew、`node_modules`、`/nix/store` 等），也拒绝覆盖一个*不同的*已有 `codewhale`。

预期输出：

```
Installing Codewhale for linux-x64
Release assets: https://github.com/codewhale-hq/CodeWhale/releases/latest/download
Install dir: /home/you/.local/bin
Checksums verified
Installed checksummed release commands:
  /home/you/.local/bin/codewhale
  /home/you/.local/bin/codew
…
PATH selects no codewhale command; this install is /home/you/.local/bin/codewhale
…
Put /home/you/.local/bin first on PATH in future shells (run once; this installer does not edit shell profiles):
  echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
Then run: . ~/.bashrc   (or open a new terminal)
…
```

### macOS 说明

在 macOS 26.1、Apple silicon（`macos-arm64`）上，用全新的 `HOME` 重新检查过：

* 安装器打印了 `Installing Codewhale for macos-arm64`，用系统自带工具校验了校验和，并在 4.3 秒内装好了 `codewhale` 和 `codew`（各 64 MiB，Mach-O arm64）。两者都报告 `codewhale 0.10.0 (1be1a703b975)`。运行时没有弹出 Gatekeeper 提示。
* 当 `PATH` 上没有 Node 时，它还会打印 `Computer Use is included and needs Node.js 20 or newer on PATH.`。核心 TUI 没有 Node 也能用，但在 `PATH` 上有 Node 之前，Computer Use 和 JavaScript 执行工具（`js_execution`）不可用。
* `codewhale doctor` 的行为与 Linux 相同（退出码 0；没有密钥时输出 `All checks complete!`；文件型密钥存储位于 `~/.codewhale/secrets/`），只是它会报告 `✓ sandbox available: macos-seatbelt`。

### 把它加入 PATH

如果安装器提示 `PATH selects no codewhale command`，说明 `~/.local/bin` **在当前 shell 里**不在 PATH 上。此时 `codewhale.net/install.sh` 安装器会根据你的 `$SHELL`（zsh、bash、fish 或 POSIX `sh`），打印下面这个代码块里对应的那一行；对于其他 shell，或者目录名含有引号、`$`、反引号或反斜杠的情况，它会让你自己添加该目录。它自己从不修改 shell 配置文件。发布压缩包内的 `install.sh` 只会打印针对当前 shell 的 `export` 行。在 Ubuntu 和 Debian 上，`~/.profile` 会加入 `~/.local/bin`，但前提是该目录在你*登录*时已经存在。所以：

* 新开的 SSH 会话或登录 shell 会自动生效；
* 桌面上新开的终端**窗口**（GNOME Terminal、Ghostty 等）通常不会，要等到你注销再登录。我刚装完就在 Ghostty 里遇到了 `bash: codewhale: command not found`。

一次性修复：

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc   # bash (macOS login bash: ~/.bash_profile, or ~/.profile if only that exists)
# echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc  # zsh
# fish_add_path ~/.local/bin                               # fish (untested on this VM)
export PATH="$HOME/.local/bin:$PATH"; hash -r
command -v codewhale codew
```

### 选项

```bash
# Choose the directory (must be absolute; created if missing)
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_INSTALL_DIR="$HOME/.local/codewhale/bin" sh
# Install a specific release
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_VERSION=v0.9.13 sh
# Show help
curl -fsSL https://codewhale.net/install.sh | sh -s -- --help
```

（注释含义：选择目录，必须是绝对路径，不存在则创建；安装指定版本；显示帮助。）

### 验证

```bash
codewhale --version     # codewhale 0.10.0 (1be1a703b975)
codew --version         # same
codewhale doctor        # diagnostics; see the note in §8 about what it does NOT check
```

### 重新运行安装器

* 已安装的就是同一版本：无害。它会打印 `Already installed: …` 并以 0 退出。
* 已安装的是不同版本：它会**拒绝**（`codewhale install: refusing to replace existing …/codewhale`），以 1 退出，并且不改动任何东西。拒绝之前它已经下载了约 160 MB。请改用 [`codewhale update`](#7-更新与回滚)。

### 升级 / 卸载

* 升级：`codewhale update`（见 §7）。
* 卸载：`rm ~/.local/bin/codewhale ~/.local/bin/codew`，数据的清理见 §12。

---

## 3. 从 GitHub Releases 手动下载

当你想亲自查看并校验每一个字节时使用。发布页：<https://github.com/codewhale-hq/CodeWhale/releases>。每个平台都有**裸二进制**（`codewhale-linux-x64`、`codew-linux-x64` 等）和一个**压缩包**（`codewhale-linux-x64.tar.gz`），压缩包里是同样的两个二进制外加一个 `install.sh`。

### 3a. 裸二进制

```bash
mkdir -p ~/codewhale-dl && cd ~/codewhale-dl
base=https://github.com/codewhale-hq/CodeWhale/releases/latest/download
curl -fsSLO "$base/codewhale-linux-x64"          # use linux-arm64 on ARM
curl -fsSLO "$base/codew-linux-x64"
curl -fsSLO "$base/codewhale-artifacts-sha256.txt"
sha256sum -c codewhale-artifacts-sha256.txt --ignore-missing
#   codew-linux-x64: OK
#   codewhale-linux-x64: OK
mkdir -p ~/.local/bin
install -m 755 codewhale-linux-x64 ~/.local/bin/codewhale
install -m 755 codew-linux-x64     ~/.local/bin/codew
```

然后[把 `~/.local/bin` 加入 PATH](#把它加入-path)并运行 `codewhale --version`。在 macOS 上，资源名是 `codewhale-macos-arm64` 和 `codew-macos-arm64`（Intel 上是 `-macos-x64`），可以用系统自带的 `shasum` 校验（已在 macOS 26.1、Apple silicon 上测试）：

```bash
/usr/bin/shasum -a 256 -c codewhale-artifacts-sha256.txt --ignore-missing
#   codew-macos-arm64: OK
#   codewhale-macos-arm64: OK
```

`codewhale-macos-arm64.tar.gz` 压缩包同样可以对照 `codewhale-bundles-sha256.txt` 校验，其中的 `./install.sh` 会安装到 `~/.local/bin`（已测试）。

要固定某个版本，把 `latest/download` 换成 `download/vX.Y.Z`，并从同一个标签下取校验清单。

### 3b. 压缩包

```bash
cd "$(mktemp -d)"
base=https://github.com/codewhale-hq/CodeWhale/releases/latest/download
curl -fsSLO "$base/codewhale-linux-x64.tar.gz"
curl -fsSLO "$base/codewhale-bundles-sha256.txt"     # note: *bundles*, not *artifacts*
sha256sum -c codewhale-bundles-sha256.txt --ignore-missing
#   codewhale-linux-x64.tar.gz: OK
tar -xzf codewhale-linux-x64.tar.gz
cd codewhale-linux-x64 && ./install.sh               # -> ~/.local/bin; PREFIX=/some/dir ./install.sh -> /some/dir/bin
```

（注意：校验清单是 *bundles*，不是 *artifacts*。）

压缩包里的 `install.sh` 与网站上的安装器行为一致：不用 sudo，不动内容不同的已有文件，并打印同样的 PATH 提示。

**升级：** `codewhale update` 对 3a 和 3b 都适用，因为它们都属于"直接二进制"安装。**卸载：** 删除那两个文件（见 §12）。

---

## 4. npm

**前置条件：** Node.js 18+ 和 npm，并且有一个**你能写入的全局 prefix**。npm 安装的是注册表上最新已发布的版本，绝不会是未发布的源码候选版。

```bash
npm install -g codewhale
codewhale --version
```

这个包是一个很小的包装器。它的 `postinstall` 步骤会下载同样的 `codewhale`/`codew` 发布二进制，对照该发布的 SHA-256 清单校验，并把 `codewhale` 和 `codew` 链接到 npm 的全局 `bin`。整个过程在测试机上用了 6 秒。

**Windows npm 会话：** Node 在整个原生会话期间仍是启动器。
`taskkill /IM node.exe` 或 `Get-Process node | Stop-Process -Force` 会影响
所有匹配的 Node 进程，也可能中断 npm 启动的 Codewhale，破坏正常终端清理。
只停止自己启动的服务器 PID、占用目标端口的进程，或使用 Codewhale 任务取消。
Windows 原生压缩包/安装器可避免 npm 父进程依赖；可选 JavaScript 工具仍可能需要
Node。Codewhale 的 Windows shell 安全底线即使在 Full Access 下也会拦住已识别的
整类 Node 进程终止命令；外部硬终止无法靠这个检查实现优雅退出。

### 如果遇到 `EACCES: permission denied`

这说明 Node 是系统级安装的（apt、`/usr/local`、`/opt`），而你的用户无权写入它的全局 prefix：

```
npm error code EACCES
npm error Error: EACCES: permission denied, mkdir '/opt/node22/lib/node_modules/codewhale'
```

**不要用 `sudo npm`。** 要么使用每用户的 Node（nvm、fnm、volta），要么把 npm 指向一个你自己拥有的目录。我测试了第二种方式：

```bash
npm config set prefix "$HOME/.npm-global"
echo 'export PATH="$HOME/.npm-global/bin:$PATH"' >> ~/.bashrc
export PATH="$HOME/.npm-global/bin:$PATH"
npm install -g codewhale
command -v codewhale codew     # ~/.npm-global/bin/codewhale, ~/.npm-global/bin/codew
```

### 说明

* npm 会隐藏下载进度。加上 `--foreground-scripts` 可以看到（`codewhale: selected GitHub Releases for v0.10.0 … done.`）。所选来源也会写入 `$(npm prefix -g)/lib/node_modules/codewhale/bin/downloads/codewhale.source`。
* 该包在磁盘上占用 157 MB。
* 在 macOS 26.1（Apple silicon，Homebrew 的 Node 25）上，安装到用户自有的 prefix（`npm install -g --prefix <dir> codewhale`）用了 3 秒，并链接了 `codewhale` 和 `codew`，两者都是 `codewhale 0.10.0 (1be1a703b975)`。
* **升级：** `npm install -g codewhale@latest`。`codewhale update` 会拒绝处理 npm 安装：它打印迁移说明，并以 1 退出，输出 `error: The package-managed executable was not changed.`。
* **指定版本：** `npm install -g codewhale@0.9.13`。
* **卸载：** `npm uninstall -g codewhale`。它只删除程序本身，不删除你的数据（见 §12）。

---

<a id="4-install-via-cargo-any-tier-1-rust-target"></a><a id="7-build-from-source"></a><a id="4-通过-cargo-安装任何-tier-1-rust-目标"></a><a id="7-从源码构建"></a>

## 5. Cargo 与从源码构建

如果你的平台没有预编译二进制，或者你想自己编译，就用这条路径。只需要一个 Cargo 包：`codewhale-cli` 会安装 `codewhale` 命令。npm 和预编译发布版还会把 `codew` 作为同一个已编译运行时的便捷名称提供；Cargo 不会创建这个别名，所以如果想用短名字，请在 shell 的 rc 文件里加上 `alias codew=codewhale`。

### 前置条件（Debian/Ubuntu）

```bash
sudo apt-get install -y build-essential pkg-config libdbus-1-dev git
# Rust via rustup (the distro's cargo is too old for this edition-2024 workspace)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustc --version            # the workspace declares rust-version = 1.89
```

（注释含义：发行版自带的 cargo 太旧，无法构建这个 edition-2024 的 workspace；workspace 声明的 rust-version 为 1.89。）

`libdbus-1-dev` **是必需的**。没有它，构建会在大约一分钟后失败：

```
error: failed to run custom build command for `libdbus-sys v0.2.7`
  The system library `dbus-1` required by crate `libdbus-sys` was not found.
```

Fedora/RHEL：`sudo dnf install -y gcc make pkgconf-pkg-config dbus-devel` **（该虚拟机上未测试：只有 Ubuntu）**。

### 5a. 从 crates.io 安装

```bash
cargo install codewhale-cli --locked
codewhale --version
```

测试结果：用当前 stable Rust（1.98.1）**可行**。
* 在 4 vCPU、15 GB 内存的机器上用了 **25 分 31 秒**，向 `~/.cargo/registry` 拉取了约 470 MB。
* 它安装一个 122 MB 的文件 `~/.cargo/bin/codewhale`。这是普通的 glibc 链接二进制，运行时需要 `libdbus-1`。
* `codewhale --version` 打印 `codewhale 0.10.0`，不带 commit 哈希。
* 无头模式和 TUI 冒烟测试通过。

> **v0.10.0 声明的"Rust 1.88+"是错的，工作区现在声明 1.89（CI 的 MSRV 任务所构建的版本）。** 用 1.88.0 时，安装会在几秒内失败：
> `rustc 1.88.0 is not supported by the following package: serde-saphyr@1.3.0 requires rustc 1.89`。
> 请使用当前 stable（`rustup update stable`）。

### 5b. 从 git 检出构建

```bash
git clone --depth 1 --branch v0.10.0 https://github.com/codewhale-hq/CodeWhale.git
cd CodeWhale
cargo install --path crates/cli --locked      # installs ~/.cargo/bin/codewhale
```

需要知道的事（实际观察到的）：

* 仓库里有 `rust-toolchain.toml`（`channel = "stable"`）。在检出目录里运行的第一条 `cargo` 命令，无论你的默认工具链是什么，都会**悄悄下载最新的 stable 工具链**（约 250 MB）。
* workspace 把所有编译器警告都当作错误。用 Rust 1.89 时，构建会在大约 11 分钟后**失败**，在 `codewhale-tui` 中出现 8 个 `error: this lint expectation is unfulfilled`。请使用仓库选定的 stable 工具链，不要传入更旧的 `+toolchain`。
* workspace 的 release profile 使用 thin LTO。在 4 vCPU / 15 GB 的虚拟机上，仅 `codewhale-tui` 这一个 crate 就编译了一个多小时，内存峰值 6–8 GB。请准备 16 GB 或更多内存，并预期这是迄今最慢的安装路径。`target/` 增长超过了 1.2 GB。

测试结果：用仓库选定的 stable Rust（1.98.1）**可行**。
* `Finished release profile … in 83m 12s`。其间大部分时间有一个 Nix 构建在争抢 CPU 和内存，所以请把它当作上限。
* `target/` 最终达到 **3.1 GB**。之后用 `cargo clean` 删除。
* 该二进制报告 `codewhale 0.10.0 (dev)`，`exec` 可用。
* Cargo 会打印 `warning: default toolchain implicitly overridden with stable-x86_64-unknown-linux-gnu by rustup toolchain file`，这是无害的。
* **Cargo 构建不提供 `codew`。**

**升级：** 重新运行同样的 `cargo install … --force`（对 crates.io 可以加 `--version X.Y.Z`）。`codewhale update` 会拒绝处理 Cargo 安装。**卸载：** `cargo uninstall codewhale-cli`，然后见 §12。

---

## 6. Linux 上的 Homebrew 与 Nix

### Linux 上的 Homebrew：可行，但不是旧文档给的那条命令

前置条件：Homebrew 本身。它的安装器需要**一次** sudo，用来创建 `/home/linuxbrew/.linuxbrew`。没有 sudo 权限时，它会停在 `Insufficient permissions to install Homebrew to "/home/linuxbrew/.linuxbrew"`。请管理员运行 `sudo mkdir -p /home/linuxbrew/.linuxbrew && sudo chown $USER /home/linuxbrew/.linuxbrew`，然后重新运行安装器。之后，按它的 "Next steps" 提示，把 `eval "$(/home/linuxbrew/.linuxbrew/bin/brew shellenv bash)"` 加入 `~/.bashrc`。

```bash
brew install Hmbown/deepseek-tui/codewhale      # full name: taps and trusts in one step
```

（注释含义：使用完整名称，一步完成 tap 和信任。）

两步写法（先 `brew tap Hmbown/deepseek-tui`，再 `brew install codewhale`）在 **Homebrew 7.x 上会失败**：

```
Error: Refusing to load formula hmbown/deepseek-tui/codewhale from untrusted tap hmbown/deepseek-tui.
Run `brew trust --formula hmbown/deepseek-tui/codewhale` or `brew trust hmbown/deepseek-tui` to trust it.
```

先运行 `brew trust hmbown/deepseek-tui`，或者使用上面的完整名称。

用 Homebrew 7.0.6 对照 v0.10.0 的 tap formula 测试过：安装用了 73 秒。tap formula 跟踪最新发布并下载官方发布二进制，所以不需要编译。它**同时**提供 `codewhale` 和 `codew`。`Hmbown/deepseek-tui` 这个 tap 的 formula 还依赖 `node`，在 Linux 上拉了 31 个 bottle（约 560 MB）。这个 `node` 依赖只属于该 tap formula。核心 TUI 没有 Node 也能运行；Computer Use 和 JS 执行工具在 PATH 上有 Node 时会使用它。

* **升级：** `brew upgrade codewhale`。（`codewhale update` 会拒绝，并建议迁移。）
* **卸载：** `brew uninstall codewhale && brew untap Hmbown/deepseek-tui`。这也会自动移除该 tap 的 node 依赖以及它拉进来的其他东西。Homebrew 的下载缓存（`~/.cache/Homebrew`，约 330 MB）会保留，直到运行 `brew cleanup --prune=all`。

### Nix：部分测试

```bash
# flakes are still experimental; the tested setup enabled them once:
mkdir -p ~/.config/nix
echo 'experimental-features = nix-command flakes' >> ~/.config/nix/nix.conf
nix run github:codewhale-hq/CodeWhale -- --version
# one-off alternative (untested on this VM): nix --extra-experimental-features 'nix-command flakes' run github:codewhale-hq/CodeWhale -- --version
```

（注释含义：flakes 仍是实验特性，测试环境一次性启用了它；一次性替代写法在该虚拟机上未测试。）

Nix 2.35 安装正常；单用户模式需要先由 root 创建一次 `/nix`。flakes 解析成功。我在停止之前了解到：

* **没有二进制缓存**，所以这是对 **main 分支**的完整源码构建，而不是 v0.10.0 发布版。该二进制报告 `codewhale 0.10.0 (dev)`。
* 构建步骤在 4 核上用了 30 分钟。之后这个包会运行它的**测试套件**（`doCheck`），以测试模式重新编译 workspace。这花了超过 70 分钟，单个 rustc 进程占用超过 8 GB 内存，我在 105 分钟时停止了。因此 `nix run` 跑完、`nix build` 以及 `nix profile install/remove` 都**（该虚拟机上未测试：构建没能在时间预算内完成；该虚拟机的代理还需要 `--override-input fenix …` 这个变通办法）**。
* Nix 只提供 `codewhale`，没有 `codew`（它只构建 `codewhale-cli` 包）。

除非你本来就用 Nix，否则请改用 §2。

---

## 7. 更新与回滚

这些适用于通过 §2 和 §3 安装的（直接二进制）。由包管理器安装的（npm、Cargo、Homebrew）必须用各自的工具更新。

```bash
codewhale update --check
#   Current binary: /home/you/.local/bin/codewhale
#   Current version: v0.9.13
#   Latest stable release: v0.10.0
#   Update available. Run `/home/you/.local/bin/codewhale update` to install v0.10.0.
codewhale update
#   Downloading codewhale-linux-x64...
#   SHA256 checksum verified against codewhale-artifacts-sha256.txt from GitHub Releases.
#   ✅ Successfully updated to v0.10.0!
#   Updated binaries:
#     - /home/you/.local/bin/codewhale (codewhale-linux-x64)
#     - /home/you/.local/bin/codew (codewhale-linux-x64)
```

它会把 `codewhale` **和** `codew` 一起更新，在测试机上用了 10 秒。再运行一次会得到 `Already up to date; no download needed.`。其他选项：`--beta` 和 `--proxy <URL>`。

<a id="roll-back-to-a-previous-release"></a>

### 回滚（例如回到 v0.9.13）

`codewhale update` 从不降级，`CODEWHALE_VERSION=0.9.13 codewhale update` 也只会说 "Already up to date"。要回滚，请替换文件：

```bash
dir="$(dirname "$(command -v codewhale)")"     # the install PATH actually selects
rm "$dir/codewhale" "$dir/codew"
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_VERSION=v0.9.13 CODEWHALE_INSTALL_DIR="$dir" sh
hash -r; codewhale --version      # codewhale 0.9.13 (a0b81f619b66)
```

（注释含义：`dir` 是 PATH 实际选中的安装目录。）

默认的 `~/.local/bin` 和自定义的 `CODEWHALE_INSTALL_DIR` 都测试过。只对通过安装器、手动下载或压缩包安装的使用它。绝不要把它指向 npm、Cargo 或 Homebrew 的目录。

或者让两个版本并存，把旧版本放在 PATH 最前面：

```bash
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_VERSION=v0.9.13 CODEWHALE_INSTALL_DIR="$HOME/.local/codewhale-0.9.13" sh
export PATH="$HOME/.local/codewhale-0.9.13:$PATH"; hash -r
```

原地回滚之后要回到最新版，运行 `codewhale update`。（已测试：0.9.13 → 0.10.0。）

npm：`npm install -g codewhale@0.9.13`。Cargo：`cargo install codewhale-cli --version 0.9.13 --locked --force` **（该虚拟机上未测试：只构建过 0.10.0）**。

---

## 8. API 密钥与提供商

### Codewhale 到哪里找密钥（先匹配到的优先）

1. 命令行上的 `--api-key <KEY>`
2. `~/.codewhale/config.toml` 中的 `api_key`
3. `codewhale auth set` 写入的密钥存储
4. 环境变量（DeepSeek 用 `DEEPSEEK_API_KEY`）

这个顺序很重要。**配置文件或密钥存储里的密钥，优先于 `DEEPSEEK_API_KEY`。** 如果你靠导出新的环境变量来轮换密钥，之前存下的旧密钥仍然会被继续使用。我测试过：`config.toml` 里是错误的密钥，环境变量里是正确的密钥，结果是 `Authentication Fails … ****beef is invalid`。

### 设置 DeepSeek 密钥的方式（都已测试）

**环境变量。** 适合试用和 CI：
```bash
export DEEPSEEK_API_KEY=sk-...          # add to ~/.bashrc / ~/.zshenv to persist
```

**`auth set`。** 为所有目录保存密钥：
```bash
codewhale auth set --provider deepseek                         # prompts: "Enter API key for deepseek:"
printf '%s\n' "$KEY" | codewhale auth set --provider deepseek --api-key-stdin   # scripted
# -> saved API key for deepseek to file-based ("/home/you/.codewhale/secrets/secrets.json") (config contains metadata only)
```
文件型存储的这条消息会打印解析出的密钥存储路径；显式设置 `CODEWHALE_HOME` 会改变这个位置。

在 Linux 上，密钥以**明文**存放在 `~/.codewhale/secrets/secrets.json` 中，权限为 0600，不在操作系统的钥匙串（keyring）里。请注意，在 v0.10.0 中，`auth set` 还会往你的配置里写入 `default_text_model = "deepseek-v4-pro"`，把你从默认的 `deepseek-flash` 切换到更贵的 Pro 模型。可以在 TUI 里用 `/model` 改回去，或者编辑 `~/.codewhale/config.toml`。

**在 TUI 里。** 按 **F3**（或输入 `/provider`），选择 DeepSeek，按 Enter，粘贴密钥（会被遮蔽），选择模型并确认。这同样会写入密钥存储，并保持使用 `deepseek-flash`。

**配置文件。** `~/.codewhale/config.toml`：
```toml
[providers.deepseek]
api_key = "sk-..."
```

### 查看当前生效的是哪个密钥

```bash
codewhale auth status --provider deepseek
#   active source: env (last4: ...xxxx)        # or: secret store / config / missing
#   lookup order: config -> secret store -> env
codewhale doctor --probe-api
#   · Testing connection...  ✓ API connection successful
```

单独运行 `codewhale doctor` 会列出它能看到的环境变量密钥（`deepseek: env_source=set via DEEPSEEK_API_KEY (value not shown; not checked offline)`），但不会读取密钥存储或验证密钥；`--probe-api` 会两者都做。即使找不到密钥，doctor 也以 0 退出，所以脚本请用 `auth status`。

### 删除已保存的密钥

```bash
codewhale auth clear --provider deepseek
#   cleared API key for deepseek from config and secret store
```

它不会取消你 shell 里的 `DEEPSEEK_API_KEY`，也会保留 `auth set` 添加的那行 `default_text_model`。

### 其他提供商

`codewhale auth list` 会列出约 50 个提供商（OpenRouter、Anthropic、OpenAI、Moonshot、Ollama 等）。用法相同：`codewhale auth set --provider <name>`，或者使用该提供商对应的环境变量。本地模型（Ollama、vLLM、SGLang）不需要密钥。这里只测试了 DeepSeek。

---

<a id="8-shell-completions"></a>

## 9. Shell 补全

```bash
# bash (needs the bash-completion package)
mkdir -p ~/.local/share/bash-completion/completions
codewhale completion bash > ~/.local/share/bash-completion/completions/codewhale

# zsh
mkdir -p ~/.zfunc
codewhale completion zsh > ~/.zfunc/_codewhale
# in ~/.zshrc, if not already there:
#   fpath=(~/.zfunc $fpath)
#   autoload -Uz compinit && compinit

# fish
mkdir -p ~/.config/fish/completions
codewhale completion fish > ~/.config/fish/completions/codewhale.fish
```

每个脚本都同时注册 `codewhale` 和 `codew`。`codewhale completions` 是别名。之后请打开一个新的 shell。升级后请重新生成。

v0.10.0 中它们的表现（交互式测试）：

* **bash：** 完全可用（`codewhale comp<Tab>`、`codew auth <Tab><Tab>`）。
* **fish：** 子命令能补全并带说明，但 `codewhale completion <Tab>` 提供的是文件，而不是 shell 名称。
* **zsh：** 只有第一个词能补全。在子命令之后（`codewhale auth <Tab>`），zsh 会错误地再次列出顶层命令（macOS 的 zsh 5.9 上同样如此，会给出全部 126 个顶层条目）。

PowerShell 和 Elvish 的脚本也会生成，**（该虚拟机上未测试：未安装这些 shell）**。

---

## 10. 运行

### TUI

```bash
cd your-git-repo
codewhale
```

* 输入框（composer）在底部。页脚显示权限级别（`ask`）、模式（`work`）和模型（`DeepSeek · deepseek-flash`）。
* **Shift+Tab** 循环切换权限级别：Ask → Auto-Review（自动审核）→ Full Access（完全访问）。（输入框为空时）**Tab** 循环切换模式：Plan → Work → Operate。
* 在 **Ask** 下，工作区内的文件编辑会被直接应用并以 diff 显示。shell 命令会停在 **APPROVAL** 提示：`y` 允许本次，`a` 在本次会话中允许，`n` 拒绝，`Esc` 中止当前回合。
* 常用按键：**F1** 帮助（或 `/help`），**Ctrl-K** 命令面板，**F3** 提供商/模型选择器，**Ctrl-R** 恢复过去的会话，**Ctrl-U** 清空输入（**Ctrl-Z** 可恢复），**Ctrl-C** 取消或退出，**Ctrl-D** 在输入为空时退出。完整列表见 [KEYBINDINGS.md](./KEYBINDINGS.md)。
* 退出时它会打印 `To resume this session, run codewhale resume <id>`。

Codewhale 会在你的仓库里创建一个 `.codewhale/` 目录。请忽略它的内容，但保留可提交的 `constitution.json`（这些规则与 `/init` 写入的相同）：

```gitignore
**/.codewhale/*
!**/.codewhale/constitution.json
```

### 无头模式（脚本、CI）

```bash
codewhale exec "Reply with exactly: pong"               # one-shot answer, no tools
codewhale exec --auto "create primes.py that prints the first 10 primes and run it"   # tools, auto-approved
codewhale exec --json "…"                               # summary JSON (provider, model, usage, output)
codewhale exec --auto --output-format stream-json "…"   # one JSON event per line
```

（注释含义：单次回答，无工具；启用工具并自动批准；汇总 JSON（提供商、模型、用量、输出）；每行一个 JSON 事件。）

`--auto` 会自动批准 shell 命令，所以只在你信任的仓库或沙箱里使用。

普通的 `exec` 不给模型提供任何工具。只有 `--auto`、`--yolo`、`--allowed-tools` 或恢复某个会话才会打开工具面；`--max-turns`、`--disallowed-tools`、`--sandbox` 和输出格式这类限制项从不会增加工具（仅对工具有效的参数会打印警告）。如果提供商在输出上限处截断了回复，会要求模型继续，最终打印出的答案是完整的回复。普通运行最多进行 8 个模型步骤，除非用 `--max-turns` 另设上限；在该上限处仍被截断的回复会使这次运行失败。

### 恢复会话

```bash
codewhale resume <session-id>     # or a unique prefix, e.g. e2525dfb
codewhale -c                      # continue the most recent session in this folder
codewhale sessions                # list saved sessions
codewhale exec --continue "…"     # headless follow-up to the latest session
codewhale exec --resume <id> "…"
```

（注释含义：会话 ID 或其唯一前缀；继续此目录中最近的会话；列出已保存的会话；对最近会话进行无头跟进。）

在 v0.10.0 中，只有 **TUI 会话**和 **`--output-format stream-json`** 的 exec 运行会被保存。普通的 `codewhale exec`/`exec --auto` 运行*不会*被保存，所以紧接着的 `exec --continue` 会失败，报 `No saved sessions found for workspace`。

---

## 11. 终端说明

### Ghostty（已测试：Linux/X11 上的 Ghostty 1.3.1）

我检查的所有项目在 Ghostty 默认配置下（`TERM=xterm-ghostty`、`COLORTERM=truecolor`）都能正常工作。截图与[安装回执](https://github.com/codewhale-hq/Codewhale/tree/37ecdfcc49bc68a9b0d058b97c3946e62c34bd31/docs/install-report/v0.10.0-2026-09-23/screenshots)保存在一起。

| 检查项 | 结果 |
|---|---|
| 颜色 / truecolor 渐变、制表符绘图、Unicode（✓ é 日本語） | ✅ |
| 调整窗口大小（1504×886 → 800×500 → 还原）时重排干净 | ✅ |
| 鼠标滚轮滚动对话记录，并带有"跳到底部"按钮 | ✅ |
| 粘贴（`Ctrl+Shift+V`），多行：会被插入，不会被发送 | ✅ |
| F1、F3、Ctrl-K、Ctrl-R、Tab、Shift+Tab、Ctrl-U/Ctrl-Z、Ctrl-C、Ctrl-D | ✅ |
| 窗口标题显示状态（`waiting on you…`、`✓ done`） | ✅ |
| 退出后终端恢复正常（回到普通屏幕、光标正常、没有鼠标上报的乱码） | ✅ |

Linux 上的 Ghostty 启动的是**非登录** shell，所以它读取 `~/.bashrc` 而不是 `~/.profile`。这就是为什么 PATH 那一行需要写进 `~/.bashrc`（§2）。

你可能会注意到，一个回合结束后，一些小圆点和淡淡的标签（例如 `other · drift`）在空白区域飘动。那是 Codewhale 装饰性的"环境生命（ambient life）"小鲸鱼，不是渲染故障。

### 其他终端

tmux 会吃掉 **F1**，所以在 tmux 里请用 `/help`。有些组合键（Ctrl-Shift-…、Ctrl-Tab）需要支持增强键盘协议的终端；[KEYBINDINGS.md](./KEYBINDINGS.md) 列出了可移植的替代按键。Windows 用户请使用 Windows Terminal **（该虚拟机上未测试）**。

---

## 12. 卸载

### 第 1 步：忘掉已保存的密钥（如果你用过 `auth set` 或 F3）

```bash
codewhale auth clear --provider deepseek
```

### 第 2 步：删除程序

| 安装方式 | 删除方法 |
|---|---|
| 安装器（§2）或手动下载（§3） | `rm ~/.local/bin/codewhale ~/.local/bin/codew`（或你的 `CODEWHALE_INSTALL_DIR`） |
| npm | `npm uninstall -g codewhale` |
| Cargo | `cargo uninstall codewhale-cli` |
| Homebrew | `brew uninstall codewhale && brew untap Hmbown/deepseek-tui`（同时会移除它的 node 依赖） |

### 第 3 步：删除数据。没有任何卸载器会替你做这件事。

| 路径 | 内容 | 所见大小 |
|---|---|---|
| `~/.codewhale/` | config.toml、**secrets/secrets.json（明文密钥）**、sessions/、logs/、catalog/（模型列表，约 5 MB）、skills/、builtin-plugins/、tasks/、automations/、crashes/、audit.log、输入框历史 | 6–7 MB |
| `~/.deepseek/snapshots/` | v0.10.0 把每个回合的**工作区副本**存在这里（一个旧路径）。里面包含你运行过它的每个仓库的内容。 | 这里为 0.2–0.6 MB；随仓库大小增长 |
| `<你用过的每个仓库>/.codewhale/` | 每个工作区的状态/锁目录 | 很小 |
| 补全文件 | `~/.local/share/bash-completion/completions/codewhale`、`~/.zfunc/_codewhale`、`~/.config/fish/completions/codewhale.fish` | – |
| 你添加的 PATH 行 | `~/.bashrc`、`~/.zshrc`、`~/.profile` | – |

```bash
rm -rf ~/.codewhale ~/.deepseek/snapshots
rmdir ~/.deepseek 2>/dev/null   # removes the parent only if it is now empty
# per-repo dirs, e.g.:
find ~ -type d -name .codewhale -prune -print     # review, then delete the ones you want
```

（注释含义：仅当父目录已为空时才会删除它；逐个仓库的目录，先查看再删除想删的。）

Codewhale 没有在 `$HOME` 和它被使用过的仓库之外写入任何东西：没有系统文件、服务或 cron 任务。（我检查了测试用户在其主目录之外拥有的每一个文件。）上面的命令只删除 `~/.deepseek/snapshots`。如果你仍在使用较早的 DeepSeek-TUI，`~/.deepseek` 里的其余部分（它的配置和会话）不会被动。

---

## 13. 故障排查

下面每一个错误，都是编写本指南时实际遇到的。

**装完后立刻出现 `bash: codewhale: command not found`。**
`~/.local/bin` 在这个终端里不在 PATH 上。见[把它加入 PATH](#把它加入-path)。

**`npm error code EACCES … permission denied, mkdir '…/lib/node_modules/codewhale'`。**
你的 Node 是系统级安装的。见 [§4](#如果遇到-eacces-permission-denied)。不要用 sudo。

**`error: DeepSeek API key not found.`（来自 `codewhale exec`）**
哪里都没有密钥。按打印出的步骤操作，或见 §8。

**TUI 显示了你的消息，却始终不回答。**
没有密钥（v0.10.0 不会提示）。按 F3 → DeepSeek → Enter → 粘贴密钥。

**`error: Responses API request failed … Authentication Fails, Your api key: ****dead is invalid`。**
密钥错误或已被吊销。运行 `codewhale auth status --provider deepseek` 查看*究竟*用的是哪个来源。请记住，配置文件和密钥存储优先于环境变量。用 `codewhale auth set --provider deepseek` 修复，或者用 `codewhale auth clear --provider deepseek` 回退到环境变量。在 TUI 里，无效的密钥会把你带到"Choose your model provider"界面，并把 DeepSeek 标为 `last check failed (authentication)`。

**`error: Network error: SSE stream request failed after HTTP/1.1 fallback: Responses API request failed. … on Windows or proxy networks, try CODEWHALE_FORCE_HTTP1=1 …`。**
尽管措辞如此，在 Linux 上这通常只意味着**无法连接到 `api.deepseek.com`**。用 `curl -sI https://api.deepseek.com` 检查（返回 `401` 是正常的，说明主机可达）。如果你在代理后面，请确认已导出 `HTTPS_PROXY`。`codewhale doctor --probe-api` 对错误的密钥和网络问题都只会说 `✗ API connection failed`。

**`codewhale install: refusing to replace existing ~/.local/bin/codewhale`。**
那里已经装了不同版本。运行 `codewhale update`，或者先删除那两个文件（见 §7 回滚），或者安装到一个全新的 `CODEWHALE_INSTALL_DIR`。

**`codewhale install: checksum mismatch for codew-linux-x64`。**
下载被损坏或被篡改。什么都没有安装。重试；如果反复出现，就不要使用镜像。

**`error: The package-managed executable was not changed.`（来自 `codewhale update`）**
你是用 npm、Cargo 或 Homebrew 安装的。请改用对应的工具更新。

**`error: failed to run custom build command for libdbus-sys`（Cargo）。**
运行 `sudo apt-get install -y libdbus-1-dev pkg-config`。

**`error: No saved sessions found for workspace …`（来自 `exec --continue`）。**
上一次运行是纯文本的 `exec`，它不会被保存。请使用 TUI，或者 `--output-format stream-json`。

**zsh 补全在第一个词之后给出错误的建议。**
v0.10.0 的已知 bug。bash 和 fish 没问题。

**获取帮助：** `codewhale doctor --json` 会生成一个不含密钥的诊断包。

---

## 附录：其他平台（本版未重测）

下面各节原样沿用自本页的上一版。它们**没有**在上文的 v0.10.0 安装测试中重新运行（不在范围内：Windows、macOS、Android/Termux、FreeBSD、中国大陆镜像），仅 [macOS 说明](#macos-说明)中提到的 macOS 路径除外。通过检查已发布的 v0.10.0 资源，发现下列内容与之矛盾（[详情](https://github.com/codewhale-hq/Codewhale/blob/37ecdfcc49bc68a9b0d058b97c3946e62c34bd31/docs/install-report/v0.10.0-2026-09-23/DOC_DEFECTS.md)，D15 和 D16）：

* `packaging/winget/` 里的 winget 清单仍停留在 0.9.6。
* v0.10.0 同时发布了 `codewhale-windows-x64.zip`（附带一个把文件复制到 `%USERPROFILE%\bin` 的 `install.bat`）和 `codewhale-windows-x64-portable.zip`；下面各节只提到了前者。
* 独立的 `codewhale.bat` 启动器只能在 x64 exe 旁边工作。

### 支持平台与发布资源

[最新稳定版](https://github.com/codewhale-hq/CodeWhale/releases/latest)发布了 Linux x64/arm64、macOS x64/arm64、Windows x64/arm64 和 Android arm64 的资源。资源存在不等于平台已通过验收。下表描述的是当前源码树的平台与次要打包支持；`latest` 安装仍然选择已发布的版本。Android/Termux 为预览状态，等待真机 QA。Linux ARM64 自 v0.8.8 起可用。Linux RISC-V 预编译暂时暂停，因为锁定的 `rquickjs-sys` 依赖没有提供 `riscv64gc-unknown-linux-gnu` 绑定。

| 平台 | 架构 | GitHub 发布资源 | npm install | `cargo install` |
| ------------ | ------------ | ----------------------------------------------------- | :---------: | :-------------: |
| Linux | x64 (x86_64) | `codewhale-linux-x64`, `codew-linux-x64` | ✅ | ✅ |
| Linux | arm64 | `codewhale-linux-arm64`, `codew-linux-arm64` | ✅ | ✅ |
| Android / Termux | arm64 (aarch64) | `codewhale-android-arm64.tar.gz`（v0.9.12 已发布；真机支持仍为预览） | ⚠️⁴ 预览版 | ⚠️⁴ 预览版 |
| Linux | riscv64 | 暂时不支持，待上游绑定落地 | ❌¹ | ❌³ |
| macOS | x64 | `codewhale-macos-x64`, `codew-macos-x64` | ✅ | ✅ |
| macOS | arm64 (M 系列) | `codewhale-macos-arm64`, `codew-macos-arm64` | ✅ | ✅ |
| Windows | x64 | `codewhale-windows-x64.exe`, `codew-windows-x64.exe` | ✅ | ✅ |
| Windows | arm64 | `codewhale-windows-arm64.exe`, `codew-windows-arm64.exe` | ✅ | ✅ |
| Linux x64 或 arm64 上的 musl（Alpine） | 原生架构 | 匹配的静态 Linux 资源 | ✅（静态） | ✅ |
| 其他 Linux（其他架构上的 musl） | — | 从源码构建 | ❌¹ | ✅² |
| FreeBSD 14+ / OpenBSD | x64, arm64 | `cargo install codewhale-cli --locked`（无预编译；见 § FreeBSD） | ❌ | ✅² |

¹ npm 包会以明确的错误退出，并引导你到这里。
² 前提是你的工具链能编译较新的 Rust workspace；见下文[从源码构建](#5-cargo-与从源码构建)。
³ RISC-V 源码构建目前需要上游 `rquickjs-sys` 的 RISC-V 绑定，或启用 bindgen 的依赖构建。
⁴ 当前的 npm 包装器能识别 Android arm64，并解析匹配的 `codewhale` 和 `codew` Android 资源。npm 安装仅对 GitHub Release 已发布了这些匹配资源的包版本有效。在 #4236 和 #4242 跟踪的真机编译、启动、审批、文件工具与更新检查完成之前，Android/Termux 路径仍为预览。

Android / Termux 与 Linux arm64 不是同一个目标。不要在 Termux 里安装 Linux 的 `codewhale-linux-arm64` 压缩包；当某个发布版或候选版发布了 Termux 专用的 Android 压缩包时请使用它，或在 Termux 内从源码构建。

当前 Linux 的 **x64 和 arm64** 资源都是**静态 musl 构建**。x64 发布路径自 v0.8.65 起使用 musl；v0.9.6 把同样的构建与静态启动检查扩展到了 arm64。这些二进制没有 glibc 依赖，可在匹配的架构上跨 Ubuntu、Debian、RHEL/CentOS 和 Alpine/musl 运行。SQLite 通过 `rusqlite` 内置，因此无需单独的 `libsqlite3` 运行时包。

#### Linux ARM64 可移植性

v0.9.6 之前的 Linux arm64 资源是 GNU libc 构建，可能继承了 Ubuntu 24.04 构建主机的 `GLIBC_2.39` 最低要求。Ubuntu 22.04 自带 glibc 2.35，因此那些较老的 arm64 二进制可能报错，例如：

```text
version `GLIBC_2.39' not found
```

npm 包装器、`codewhale update` 和 Unix 压缩包安装器对较旧版本仍保留 GNU 二进制预检查。当前的 arm64 构建改用 `aarch64-unknown-linux-musl`，因此没有 `GLIBC_*` 最低要求。如果你要在较旧的 arm64 发行版上安装早期版本，请使用：

```bash
cargo install codewhale-cli --locked   # installs `codewhale`
```

> **Linux ARM64 说明（v0.8.7 及更早）。** v0.8.7 及更早版本**没有**发布 Linux ARM64 预编译；使用 HarmonyOS 轻薄本、Asahi Linux、树莓派（Raspberry Pi）、AWS Graviton 等的用户，运行 `npm i -g codewhale` 时会看到 `Unsupported architecture: arm64`。v0.8.8 发布了 `codewhale-linux-arm64`，因此普通的 `npm i -g codewhale` 可在任何基于 glibc 的 ARM64 Linux 上工作。如果你还卡在 v0.8.7，请跳到[从源码构建](#5-cargo-与从源码构建)——`cargo install` 完全可用。HarmonyOS PC 与 OpenHarmony 交叉构建设置，见 [HarmonyOS 与 OpenHarmony](./HarmonyOS.md)。

<a id="migrating-from-npm-cargo-or-another-installation"></a>

### 从 npm、Cargo 或其他安装迁移

包管理器继续拥有自己的文件。对于 npm、Cargo、Homebrew 和 Omarchy，`codewhale update` 会给出迁移说明，而不是覆盖它们。已知的系统/包目录也受到保护。`CODEWHALE_INSTALL_METHOD=binary` 覆盖项无法绕过已识别的受管路径。

当 `~/.local/bin` 已被占用，或者同级命令的内容不同，就创建一个全新的目标目录。这样每个已有的安装都原样保留：

```bash
mkdir -p "$HOME/.local"
codewhale_install_dir="$(mktemp -d "$HOME/.local/codewhale-release.XXXXXX")"
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_INSTALL_DIR="$codewhale_install_dir" sh
"$codewhale_install_dir/codewhale" --version
export PATH="$codewhale_install_dir:$PATH"
hash -r
command -v codewhale codew
"$codewhale_install_dir/codewhale" update --check
```

确认版本和命令路径之后，把该目录放在 shell 配置里 PATH 的最前面。在 PowerShell 里，用 `Get-Command codewhale, codew -All` 检查解析结果；运行所选可执行文件时使用它的完整路径。一次成功的更新只会改动它自己所在的安装目录，所以 PATH 上更靠前的其他条目仍可能启动旧副本。

现代的、成对匹配的 `codewhale`、`codew` 以及兼容性副本，都从同一份已校验的字节更新。指向正在运行的二进制的符号链接会被保留。内容不同或不相关的同级文件会在错误里被点名并原样保留；不会仅仅为了猜测其归属而执行任何同级文件。对于带有独立 dispatcher/TUI 二进制的旧安装，请使用上面的新目录迁移方式。

如果要保留一份由包管理器管理的次要安装，请用它自己的管理器：

```bash
npm install -g codewhale@latest
# or
cargo install codewhale-cli --locked --force
```

Homebrew 用 `brew upgrade codewhale`；Omarchy 用 `omarchy update`。这些命令只更新它们各自的副本，所以之后请再次核对 PATH。

<a id="android--termux-arm64"></a>

### Android / Termux arm64（预览）

Termux 运行在 Android 的 Bionic libc 上，并使用 `$PREFIX` 作为它的 Unix 前缀，因此需要 Termux 专用的 Android arm64 压缩包。Linux arm64 发布资源面向使用 musl 的标准 Linux；Android 使用不同的 Rust 目标，所以不应在那里使用 Linux 资源。

先安装最基本的压缩包/运行时工具：

```bash
pkg update
pkg install -y ca-certificates curl tar gzip coreutils
```

当发布版包含 `codewhale-android-arm64.tar.gz` 时，用压缩包自带的安装器安装。传入 `PREFIX="$PREFIX"` 很重要：安装器默认安装到 `~/.local`，而 Termux 用户通常期望命令在 `$PREFIX/bin` 下。

```bash
cd "$HOME"
curl -L -O https://github.com/codewhale-hq/CodeWhale/releases/latest/download/codewhale-android-arm64.tar.gz
curl -L -O https://github.com/codewhale-hq/CodeWhale/releases/latest/download/codewhale-bundles-sha256.txt
sha256sum -c codewhale-bundles-sha256.txt --ignore-missing

tar xzf codewhale-android-arm64.tar.gz
cd codewhale-android-arm64
PREFIX="$PREFIX" ./install.sh
hash -r
```

如果你要从源码验证，或在本地构建候选版，请在运行 Cargo 之前先安装构建包：

```bash
pkg install -y rust clang pkg-config make git
cargo install codewhale-cli --locked   # installs `codewhale`
```

常规的首次运行设置流程已经实现，但它在 Android 上的交互仍属于上文提到的预览 QA 范围。临时凭据优先使用提供商的环境变量。`codewhale auth set` 可用，但 Termux 构建没有受支持的操作系统钥匙串（keyring）集成，会退化为文件型密钥：写入 `~/.codewhale/config.toml`，并把密钥镜像到 `~/.codewhale/secrets/secrets.json`。两者都是受 `0600` 权限保护的明文文件，静态存储时未加密。

```bash
codewhale auth set --provider deepseek
codewhale auth status
codewhale doctor
```

维护者应对 Termux / Android arm64 候选版使用这份可重复的冒烟检查清单：

```bash
command -v codewhale codew
test -x "$PREFIX/bin/codewhale"
test -x "$PREFIX/bin/codew"

codewhale --version
codewhale doctor
codewhale exec --auto "run pwd"
```

已知限制：

- 命令会继承 Android 的每应用 UID、SELinux 和 seccomp 保护，以及授予 Termux 的任何权限。Codewhale 的 bubblewrap 子进程沙箱仅限 Linux，没有在 Android 上构建，因此已批准的命令不会获得 Codewhale 特有的文件系统限制。
- Termux 构建没有受支持的 Android Keystore 或桌面 Secret Service 集成。用 `codewhale auth status` 确认当前生效的来源；当文件型明文存储不可接受时，优先使用提供商的环境变量。
- 终端渲染因 Android 终端应用而异。TUI 始终独占备用屏幕（alternate screen）。如果某个终端应用无法渲染全屏 TUI，请改用 `codewhale exec` 进行无头运行。

### 中国大陆/镜像友好安装

从中国大陆安装时，请同时为 **rustup**（Rust 工具链安装器）和 **Cargo**（包注册表）配置镜像，以避免 TLS 超时和下载失败。

**第 1 步：通过 rustup 镜像安装 Rust**

```bash
# PowerShell
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
(New-Object Net.WebClient).DownloadFile('https://win.rustup.rs/x86_64', 'rustup-init.exe')

# git-bash / msys2
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
export RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup
./rustup-init.exe -y --default-toolchain stable

# Linux / macOS
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
export RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
```

如果 TUNA 镜像在你的网络下很慢，`rsproxy.cn` 是 Linux/macOS 的另一个 rustup 镜像选择：

```bash
export RUSTUP_DIST_SERVER=https://rsproxy.cn
export RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
```

`RUSTUP_DIST_SERVER` 和 `RUSTUP_UPDATE_ROOT` 环境变量**必须**在运行 rustup-init **之前**设置；否则工具链下载会遇到与安装器相同的 TLS 握手问题。

**第 2 步：配置 Cargo 注册表镜像**

```toml
# ~/.cargo/config.toml
[source.crates-io]
replace-with = "tuna"

[source.tuna]
registry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"
```

`rsproxy`、腾讯云 COS 和阿里云 OSS 镜像的用法相同；选你的网络下最快的即可。

### Omarchy / AUR

在 Omarchy 上，安装预构建的 AUR 包：

```bash
omarchy pkg aur add codewhale-bin
codewhale --version
```

`codewhale-bin` 打包的是与其他二进制安装路径相同的、校验和已固定的 Linux 发布压缩包，并同时提供 `codewhale` 和 `codew`。它不携带单独的 Codewhale 版本；现有的 `codewhale-tui` 兼容命令仍是同一运行时的别名。包更新通过 `omarchy update` 到达；应用内更新器会把 pacman 拥有的二进制留给 Omarchy 处理。

AUR 更新跟随对应的 Codewhale 标签和发布资源，所以它可能在 GitHub 发布之后才出现，因为其生成的 `PKGBUILD` 和 `.SRCINFO` 需要先经过验证。发布维护者说明见 [`packaging/aur/README.md`](../../packaging/aur/README.md)。

---

### Windows

#### Windows Scoop

`codewhale` 包列在 Scoop 的 main bucket 中：

```powershell
scoop update
scoop install codewhale
codewhale --version
```

Scoop 清单维护在本仓库的发布工作流之外，可能落后于 GitHub/npm/Cargo 的发布。当你需要立即拿到最新版本时，请使用 npm 或从 GitHub 手动下载发布资源。

#### Windows winget

已发布的 winget 包是 **`HunterBown.CodeWhale`**（2026-10-04 在
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs/tree/master/manifests/h/HunterBown/CodeWhale)
核实；最新发布版本为 0.10.0）。它是便携式 x64 包：winget 下载
`codewhale-tui-windows-x64.exe`，将其安装为 `codewhale` 命令，并安装 Microsoft Visual C++ 2015+ x64 运行库。

```powershell
winget install HunterBown.CodeWhale
codewhale --version
```

使用 `winget upgrade HunterBown.CodeWhale` 或 `codewhale update` 更新。每个版本在 GitHub Release 之后都要经过 winget-pkgs 审核，因此 winget 可能落后于 GitHub/npm/Cargo；需要立即获得最新版本时，请使用 npm 或 GitHub Release 资源。

winget 方式的已知限制：

* **仅 x64。** 已发布的清单没有 ARM64 安装包。在 Windows ARM64 上，请在原生 ARM64 Node.js 下运行 `npm install -g codewhale`，或从 GitHub Releases 下载 `codewhale-windows-arm64.zip`。
* **仅安装 `codewhale`。** winget 不安装简短别名 `codew`；请使用 `codewhale`。
* 本仓库中的清单（`packaging/winget/`）不是 winget 实际提供的清单；见 [`packaging/winget/README.md`](../../packaging/winget/README.md)。

#### Windows NSIS 安装器

从 v0.8.50 起，为喜欢传统双击安装的 Windows 用户提供了独立的、基于 NSIS 的安装器（无需 npm、Scoop 或 Cargo）。

NSIS 安装器目前包含 Windows x64 二进制。Windows ARM64 用户应通过在原生 ARM64 Node.js 下运行的 npm 安装，或从同一个发布下载 `codewhale-windows-arm64.zip`；这两条路径使用的都是原生 ARM64 二进制。

从[发布页](https://github.com/codewhale-hq/CodeWhale/releases/latest)**下载** `CodeWhaleSetup.exe`。

双击安装程序即可**安装**。安装器会：

- 把 `codewhale.exe` 和 `codew.exe` 并排安装（单二进制，没有 `codewhale-tui.exe`）到 `%LOCALAPPDATA%\Programs\CodeWhale\bin`
- 安装 `codewhale.bat`：`PATH` 上有 Windows Terminal（`wt.exe`）时优先使用它，否则直接启动 exe
- 创建当前用户的开始菜单快捷方式，指向该启动器，而不是原始的 `.exe`
- 把安装目录加入**当前用户**的 `PATH`
- 注册到 Windows 的**应用和功能（Apps & Features）**，便于卸载

卸载会移除二进制、`codewhale.bat`、开始菜单快捷方式和用户 `PATH` 条目。

**静默安装**（供 IT 管理员、SCCM、Intune 使用）：

```powershell
CodeWhaleSetup.exe /S
```

该安装器是每用户安装，不会请求提权。请在目标用户的环境中运行静默安装，或使用能为每个需要 Codewhale 的用户配置文件运行安装器的部署工具。

发布版构建的安装器目前未签名，可能触发 Windows SmartScreen。部署前请用 `codewhale-artifacts-sha256.txt` 校验 SHA-256 校验和；如果你的环境要求已签名的应用包，请在内部部署流水线中对安装程序签名。

**自行构建安装器**（需要 [NSIS](https://nsis.sourceforge.io)）：

```powershell
cd scripts\installer
# Place codewhale.exe and codew.exe here (single binary, no codewhale-tui.exe), then:
makensis /DVERSION=<version> codewhale.nsi
```

（注释含义：把 codewhale.exe 和 codew.exe 放到这里，单二进制，没有 codewhale-tui.exe，然后运行 makensis。）

**手动回退**——如果安装器被组策略阻止，请参见 [CLASSROOM_INSTALL.md](../CLASSROOM_INSTALL.md) 指南中的分步 PowerShell 命令。

> **要部署到教室或实验室？** 请参见完整的[教室安装清单](../CLASSROOM_INSTALL.md)，其中涵盖静默安装、API 密钥配发、系统镜像（imaging）说明和故障排查。

<a id="freebsd"></a>

### FreeBSD、交叉编译、Windows 源码构建

#### FreeBSD 14+ 源码构建替代方案（#1097）

FreeBSD 没有预编译的 GitHub Release 资源——`npm install -g codewhale` 会有意失败，提示 `Unsupported platform: freebsd` 并指向 Cargo。请从源码安装：

```bash
pkg install -y rust pkgconf git
cargo install codewhale-cli --locked   # installs `codewhale`
codewhale --version
codewhale doctor
```

`rquickjs` 的 FreeBSD 绑定在构建时通过 `bindgen` 生成（见 `1582ba965`/`5eb0385e8`）。目前还没有单独的 `pkg install codewhale` 端口——原生端口作为 #1097 的后续工作，记录在 `packaging/freebsd/` 之下（欢迎贡献）。请在 release 分支上用 `cargo check --target x86_64-unknown-freebsd -p codewhale-cli --locked` 验证；7×1 发布矩阵（Linux musl x64/arm64、Android arm64、macOS x64/arm64、Windows x64/arm64）仍是 7 个目标——FreeBSD 是源码构建目标，不是预编译资源。

#### 从 x64 交叉编译到 ARM64 Linux

发布资源使用 `aarch64-unknown-linux-musl`，并在原生 ARM runner 上构建。如果你想在 x64 Linux 主机上构建一个 GNU 链接的 ARM64 Linux 二进制（例如给 HarmonyOS / openEuler ARM64 轻薄本用），请使用 [`cross`](https://github.com/cross-rs/cross)，它把官方的 Rust 交叉目标封装在 Docker 容器里：

```bash
# Once
rustup target add aarch64-unknown-linux-gnu
cargo install cross --locked

# Per build
cross build --release --target aarch64-unknown-linux-gnu -p codewhale-cli   # single binary
```

生成的二进制位于 `target/aarch64-unknown-linux-gnu/release/codewhale`。把它复制到 ARM64 主机（例如通过 `scp`）并赋予可执行权限。这个本地 GNU 构建不同于可移植的 musl 发布资源；两种可执行文件都可以以 `codew` 这个便捷名称复制使用。

如果你没有 Docker，可以直接安装交叉链接器，让 Cargo 来完成工作：

```bash
sudo apt-get install -y gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu

cat >> ~/.cargo/config.toml <<'EOF'
[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"
EOF

cargo build --release --target aarch64-unknown-linux-gnu -p codewhale-cli   # single binary
```

交叉编译时生成 `aarch64-unknown-linux-musl` 需要合适的 musl 交叉链接器。发布工作流通过在 GitHub 的原生 ARM runner 上构建并启动 musl 二进制，避免了这个额外的麻烦环节。

#### Windows 源码构建

在 Windows 上构建需要 [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022) 中的 **MSVC C 工具链**（免费的、可按工作负载选择的安装器，不是完整的 IDE）。

**前置条件（Windows）**

1. 安装 Visual Studio 2022 Build Tools——选择 **"使用 C++ 的桌面开发（Desktop development with C++）"** 工作负载。
2. 安装 [Rust](https://rustup.rs) 1.89+（如果从中国大陆下载，参见上文[中国大陆/镜像友好安装](#中国大陆镜像友好安装)）。
3. 安装 [Git for Windows](https://git-scm.com/download/win)（提供 `git` 和 `git-bash` 终端）。

**推荐的终端**：Windows Terminal、`git-bash` 或 PowerShell。`cmd.exe` 可用，但缓冲区较小，PATH 行为也有限。

**设置 MSVC 环境**

Visual Studio Build Tools 会把 `cl.exe` 安装到带版本号的目录，但**不会**把它全局加入 `PATH`。你必须手动设置环境，或使用开发者命令提示符。所需的变量是：

```powershell
# Adjust version numbers to match your installation
$msvc = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207"
$sdk   = "C:\Program Files (x86)\Windows Kits\10"
$sdkv  = "10.0.26100.0"

$env:INCLUDE  = "$msvc\include;$msvc\atlmfc\include;$sdk\Include\$sdkv\ucrt;$sdk\Include\$sdkv\um;$sdk\Include\$sdkv\shared"
$env:LIB      = "$msvc\lib\x64;$msvc\atlmfc\lib\x64;$sdk\Lib\$sdkv\ucrt\x64;$sdk\Lib\$sdkv\um\x64"
$env:LIBPATH  = "$msvc\lib\x64;$msvc\atlmfc\lib\x64"
$env:CC       = "$msvc\bin\Hostx64\x64\cl.exe"
$env:CXX      = "$msvc\bin\Hostx64\x64\cl.exe"
$env:PATH     = "$msvc\bin\Hostx64\x64;$env:PATH"
```

（第一行注释含义：请调整版本号以匹配你的安装。）

或者，打开 **"VS 2022 开发者命令提示符（Developer Command Prompt for VS 2022）"**（安装 Build Tools 后可在开始菜单找到），它会运行 `vcvars64.bat` 自动配置上述所有内容。然后在该会话里把 `cargo` 加入 `PATH`，并从项目根目录运行 `cargo build`。

**Cargo 注册表镜像**——在 Windows 上，镜像配置放在 `%USERPROFILE%\.cargo\config.toml`。参见[上文第 2 步](#中国大陆镜像友好安装)。

**构建**

```bash
git clone https://github.com/codewhale-hq/CodeWhale.git
cd CodeWhale
set CARGO_HTTP_CHECK_REVOKE=false   # may be needed behind some Chinese ISPs
cargo build --release
```

（注释含义：在某些中国 ISP 之后可能需要。）

Cargo 构建出的二进制位于 `target\release\codewhale.exe`。发布打包会另外把同一个可执行文件作为 `codew.exe` 提供。

> 不想构建？通过 npm、Cargo、GitHub Releases 或 CNB 镜像安装——见上面各节。

### 旧版本与地区性故障排查

#### `Unsupported architecture: arm64 on platform linux`

你使用的是早于 v0.8.8 的版本，它不发布 Linux ARM64 二进制。请按上文所述，使用 GitHub 安装器安装到一个全新的目录，或者按[第 5 节](#5-cargo-与从源码构建)使用 `cargo install`。

#### 升级旧安装后出现 `MISSING_COMPANION_BINARY`

当前的单二进制在进程内运行 TUI，不需要配套的可执行文件。这个错误说明存在一个过时的、早于 v0.9.5 的 dispatcher。请使用上文的新目录 GitHub 迁移方式，然后核对所选的 `codewhale` 和 `codew` 路径。不要再下载另一个单独的运行时。

#### `codewhale update` 报告 `no asset found for platform codewhale-linux-aarch64`

旧版更新器使用的 Rust 架构名与发布资源名不一致。请按上文所述，用官方安装器安装到一个全新的目录，然后通过完整路径运行新安装的命令。

#### 中国大陆 npm 下载慢或超时

在 Linux x64 上，npm 包装器已经并行探测 GitHub Releases 和 CNB 第一方校验和清单，并且只从第一个通过校验的来源下载二进制。这条自动路径不需要 `CODEWHALE_USE_CNB_MIRROR=1`。

如果两个第一方来源都失败，请把 `CODEWHALE_RELEASE_BASE_URL` 设置为镜像的发布资源目录（rsproxy、TUNA、腾讯云 COS、阿里云 OSS），或者完全跳过 npm，使用[第 5 节](#5-cargo-与从源码构建)里的 Cargo 镜像设置。旧的 `DEEPSEEK_TUI_RELEASE_BASE_URL` 名称仍被接受。`CODEWHALE_USE_CNB_MIRROR=1` 仍然只在 Linux x64 / OpenHarmony x64 上强制使用 CNB。

#### 中国大陆无法从 GitHub 使用 `codewhale update`

`codewhale update` 优先使用 GitHub Releases。在受支持的 Linux x64 目标上，GitHub 清单请求失败时，允许回退到对应的 CNB 清单和二进制。如果 GitHub 元数据也无法访问，请显式选择一个已知已发布的 CNB 版本（`CODEWHALE_USE_CNB_MIRROR=1 CODEWHALE_VERSION=X.Y.Z codewhale update`），或使用下面的二进制镜像。已有的较新构建会被保留。

用 Cargo 从 CNB 源码镜像构建是次要选项。Cargo 会安装它自己管理的 `codewhale` 命令：

要在不下载、不替换二进制的情况下检查最新发布，运行 `codewhale update --check`。

```bash
cargo install --git https://cnb.cool/codewhale.net/codewhale --tag vX.Y.Z codewhale-cli --locked --force   # single binary
```

如果你运营一个二进制资源镜像，`codewhale update` 可以直接使用它：

```bash
CODEWHALE_RELEASE_BASE_URL=https://your-mirror.example.com/CodeWhale/vX.Y.Z/ \
CODEWHALE_VERSION=X.Y.Z \
codewhale update
```

镜像目录必须包含 `codewhale-artifacts-sha256.txt` 以及来自 GitHub 发布的各平台二进制。旧的 `DEEPSEEK_TUI_RELEASE_BASE_URL` 镜像变量仍作为别名受支持。

`codewhale update` 只接受 HTTPS，允许 GitHub 发布主机、CNB 镜像以及
`CODEWHALE_RELEASE_BASE_URL` 指定的主机；每一次重定向也检查相同规则。
私有镜像若重定向到另一个 CDN 或对象存储域名，还需显式列出该下载主机：

```bash
CODEWHALE_UPDATE_ALLOWED_HOSTS=cdn.your-mirror.example.com,objects.example.net \
CODEWHALE_RELEASE_BASE_URL=https://your-mirror.example.com/CodeWhale/vX.Y.Z/ \
CODEWHALE_VERSION=X.Y.Z \
codewhale update
```

被拒绝的主机和允许主机变量会出现在错误信息里。不要关闭校验或改用 HTTP。

### Windows 与 npm 下载故障排查

#### Windows：`rustup-init` 报 `TLS handshake eof` 或 `CRYPT_E_REVOCATION_OFFLINE`

在防火长城（GFW）或某些中国 ISP 之后，到 `static.rust-lang.org` 的 TLS 握手会失败。请在运行安装器**之前**设置 rustup 镜像环境变量：

```bash
# git-bash / msys2
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
export RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup
./rustup-init.exe -y --default-toolchain stable
```

如果 Rust 装好之后 Cargo 报 `CRYPT_E_REVOCATION_OFFLINE`，请在 `cargo build` 期间同时设置 `CARGO_HTTP_CHECK_REVOKE=false`。

#### Windows：`cargo build` 期间找不到 MSVC 编译器（`cl.exe`）

Visual Studio Build Tools 不会把 `cl.exe` 加入全局 `PATH`。二选一：

1. 从开始菜单打开 **"VS 2022 开发者命令提示符"**，在该窗口里把 `%USERPROFILE%\.cargo\bin` 加入 `PATH`，并从那里运行 `cargo build`；或
2. 手动设置 MSVC 环境变量——PowerShell 片段见 [Windows 源码构建](#windows-源码构建)一节。

验证编译器可用：`cl.exe /?` 应该会打印帮助文本。

#### Windows：Cargo 执行构建脚本时报 `拒绝访问 (os error 5)`

第三方杀毒软件（火绒、360、卡巴斯基等）可能会阻止 Cargo 执行刚编译出来的构建脚本二进制（例如 `libsqlite3-sys`、`aws-lc-sys`、`instability`）。这个错误与路径无关——移动 `target-dir` 也没有帮助。

**症状**：`could not execute process ... build-script-build (never executed)`

**变通办法**（任选其一）：

1. **把项目的 `target/` 目录加入杀毒软件的排除列表。**
2. **在 `cargo build` 期间暂时关闭杀毒软件。**
3. **改用 GitHub Release 安装器/压缩包**——发布资源提供预编译二进制，完全跳过 Cargo 构建（[第 3 节](#3-从-github-releases-手动下载)）。
4. **使用 crates.io 的 `cargo install codewhale-cli --locked`**——这会改变二进制路径，某些杀毒软件对不同路径的处理方式不同。

要验证构建脚本二进制本身是否有效（没有损坏），在 `target/debug/build/<crate>/build-script-build` 下找到它并手动运行：

```bash
target/debug/build/libsqlite3-sys-*/build-script-build
# If this runs but panics with "NotPresent" (no C compiler), the binary is
# fine — the AV is blocking Cargo's process-spawning path specifically.
```

（注释含义：如果它能运行，但以 "NotPresent"（没有 C 编译器）panic，说明这个二进制没问题——是杀毒软件专门在阻止 Cargo 的进程派生路径。）

#### npm 二进制下载超时

如果 `codewhale` 等待几秒后，在从 `github.com` 拉取时打印 `connect ETIMEDOUT` 或 `EAI_AGAIN`，说明 npm 包装器安装成功了，但预编译二进制的下载在你的网络上被屏蔽或不稳定。这次下载与 npm 注册表的包下载是分开的。在 Linux x64 上，包装器会先并行探测体积很小的 GitHub 和 CNB 校验清单，不会等到完整的 GitHub 二进制超时，才去使用有效的 CNB 清单。

请使用以下路径之一：

1. 设置代理并重试：

   ```bash
   export HTTPS_PROXY=http://your-proxy:port
   codewhale
   ```

2. 在内部镜像发布资源，并设置 `CODEWHALE_RELEASE_BASE_URL`：

   ```bash
   export CODEWHALE_RELEASE_BASE_URL=https://your-mirror.example.com/CodeWhale/
   codewhale
   ```

   该目录必须包含 `codewhale-artifacts-sha256.txt` 以及来自 GitHub 发布的各平台二进制。

3. 通过 Cargo 安装，它在本地构建，不下载 GitHub 发布资源。见[第 5 节](#5-cargo-与从源码构建)。

4. 从[发布页](https://github.com/codewhale-hq/CodeWhale/releases)下载匹配的 `codewhale` 和 `codew` 两个二进制，放到 `PATH` 上的某个目录并赋予可执行权限。见[第 3 节](#3-从-github-releases-手动下载)。
