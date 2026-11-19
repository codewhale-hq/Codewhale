# CNB Cool 镜像

> 英文原文：[CNB_MIRROR.md](../CNB_MIRROR.md)。
> 最后与英文同步日期（last synced with English revision）：2026-10-06。

`cnb.cool/codewhale.net/codewhale` 是这个 GitHub 仓库的单向镜像，服务那些
GitHub 慢或无法访问的网络环境（主要是中国大陆）。镜像会收到：`main` 的每一次
推送，以及每个完整 GitHub Release 已发布后的 `v*` 发布标签。GitCode 镜像
（`gitcode.com/HunterBown/Codewhale`）通过
[`Sync to GitCode`](../../.github/workflows/sync-gitcode.yml) 收到相同的 `main`
和发布标签；它只提供源码，没有二进制 release。

## 源码与下载分别验证（2026-10-06 审核）

本次审核确认 CNB `main` 与权威 GitHub `main` 同步；这不等于二进制发布完成。
GitHub、npm 与 npmmirror 的最新已发布版本为 **0.10.0**，两个 npm 注册表的
包装器压缩包字节与完整性值一致。CNB 最新二进制 release 仍是 **0.9.9**；
`v0.10.0` 的源码标签存在，但其标签构建在发布说明检查处失败，编译和上传未完成。
该版本 CNB 校验清单返回 404，所以不能宣称 `CODEWHALE_USE_CNB_MIRROR=1`
可安装 0.10.0。0.10.1 在发布前仅是 `main` 上的源码候选版。

先验证标签指向相同源码，再验证该版本的 release、资产和校验清单实际存在。
只看 GitHub 同步工作流成功，或看到源码标签，都不足以证明下载可用。
恢复已存在标签不会产生一次新的标签推送；维护者需单独恢复 CNB 构建/发布并审核
结果，不能移动或重打已经发布的标签。发布之后应重新审核本段版本状态。

## 来源

**GitHub 是唯一的权威源。** 所有发布、标签和源代码都源自
`github.com/codewhale-hq/CodeWhale`。CNB 镜像是 `Sync to CNB` 工作流维护的只读副本，
它存在的唯一目的，是服务 GitHub 被 GFW 封锁或连接缓慢的用户。

每个 CNB 发布都附带 `codewhale-artifacts-sha256.txt`，这是 CNB 构建的
Linux x64 二进制的 SHA256 清单，由 GitHub 上打标签的同一个源码提交生成。
（CNB 从源码构建，所以这些校验和对应的是 CNB 构建的产物，不是 GitHub 的发布
资产。）用它校验下载下来的二进制文件：

```bash
# Verify a downloaded CNB binary against the CNB manifest
sha256sum -c codewhale-artifacts-sha256.txt --ignore-missing
```

## 工作方式

镜像由 [`Sync to CNB`](../../.github/workflows/sync-cnb.yml) GitHub Actions
工作流维护：

- **触发：** 推送到 `main`、权威 Release 发布成功后的发布工作流、每小时第 17 和
  第 47 分钟的定时任务，或用于手动恢复的 `workflow_dispatch`。定时任务会重新镜像
  `main`（补上没有触发工作流的推送），并在最新 GitHub Release 标签在 CNB 上缺失、
  指向不同提交，或其 CNB 校验清单无法访问时失败；它从不推送标签。仅推送标签不会触发发布标签镜像。手动恢复标签也必须
  验证 GitHub 已公开的完整资产清单及不可变的源码提交。
- **认证：** HTTPS basic auth，用户名为 `cnb`，密码是仓库 secret
  `CNB_GIT_TOKEN`。fork 永远不会继承这个 secret，所以 `sync` 作业只在仓库所有者为
  `codewhale-hq` 时运行；在 fork 上会被跳过而不是失败。本仓库中 secret 缺失或拼写
  错误时，push 步骤仍会失败。
- **范围：** 只推送触发本次运行的那个 ref。标签推送就推送该标签；分支同步推送
  `main`。其他分支和 dependabot ref 有意*不*镜像。
- **并发：** 按 `cnb-sync-${{ github.ref }}` 为每个 ref 单独串行化。
  不同标签不会互相挤掉待运行任务；同一分支只保留最新待同步推送。
- **重试：** 每次推送最多重试三次，采用线性退避（5s、10s），之后工作流放弃。

CNB 流水线配置也纳入 GitHub 的源码管理，位置是
[`/.cnb.yml`](../../.cnb.yml)。这是刻意的：同步工作流会把 GitHub 的 ref 强制
镜像到 CNB，所以只在 CNB 侧创建的流水线文件会被覆盖。请通过 GitHub PR 提交
`.cnb.yml` 改动，让单向镜像把它们带到 CNB。

## CNB 标签发布

当 CNB 收到 `v*` 标签时，根目录的 `.cnb.yml` 标签流水线会从源码构建 Linux x64
发布资产，并发布一个 CNB release，包含：

- `codewhale-linux-x64`
- `codew-linux-x64`
- `codewhale-tui-linux-x64`（仅作兼容用的发布文件名，不是第三个已安装命令）
- `codewhale-artifacts-sha256.txt`

这样，能访问 CNB 但访问不了 GitHub 的用户就有了一条 CNB 原生的发布路径。
GitHub 仍是 macOS/Windows 的权威发布矩阵；CNB 标签流水线则是更适配中国网络的
Linux x64 备用路径。
GitHub 发布工作流只会在完整且经过验证的资产集公开后调用镜像。权威发布失败时，
CNB 不会提前发布该版本。已有 CNB 标签必须与源码一致；恢复不会强制改写发布标签。

## CNB 上的 Linux CI 与发布预检

第一方的 `fix/*` 和 `rebrand/*` 分支会镜像到 CNB，好让重量级的 Linux Rust 门禁
跑在腾讯托管的 runner 上，而不是 GitHub Actions：

- `./scripts/release/check-versions.sh`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test --workspace --all-features --locked`
- `cargo build --release --locked -p codewhale-cli --bin codewhale`
- `node scripts/release/npm-wrapper-smoke.js`

匹配 `work/v*` 的发布分支还会运行
`./scripts/release/publish-crates.sh dry-run`。GitHub Actions 保留轻量的
漂移/格式化状态检查，以及 CNB 无法替代的 macOS 和 Windows 任务。

## 发布后验证镜像

`release.yml` 为 `vX.Y.Z` 标签跑完之后，CNB 镜像上应该既有 `main` 的新提交，
也有这个新标签：

```bash
# Quick check: does the new tag exist on CNB?
git ls-remote https://cnb.cool/codewhale.net/codewhale.git \
    refs/tags/vX.Y.Z

# Quick check: is CNB's main at the same commit as origin/main?
gh_main=$(git ls-remote https://github.com/codewhale-hq/CodeWhale.git refs/heads/main | awk '{print $1}')
cnb_main=$(git ls-remote https://cnb.cool/codewhale.net/codewhale.git refs/heads/main | awk '{print $1}')
test "$gh_main" = "$cnb_main" && echo "in sync" || echo "DIVERGED: gh=$gh_main cnb=$cnb_main"
```

或者直接查看工作流运行：

```bash
gh run list --workflow=sync-cnb.yml --repo codewhale-hq/CodeWhale --limit 5
```

如果该发布标签最近一次运行是 `success`，说明镜像已经跟上。如果是 `failure`，
先修复或重跑镜像工作流，再引导用户去用镜像上的标签。

## 手动兜底

手动修复镜像仅限维护者。不要把 PAT 写进远程 URL，也不要在面向贡献者的文档里
发布强推（force-push）的操作步骤。尽量使用已配置的 GitHub Actions secret 和
workflow dispatch 路径。

### 手动重新触发工作流

如果工作流本身健康，只是这次发布运行碰巧失败（比如 CNB 当时短暂故障，
现在已经恢复），不推送任何东西也可以重新触发：

```bash
# Prefer rerunning the existing failed tag run when one exists.
gh run rerun <failed-tag-run-id> --repo codewhale-hq/CodeWhale

# If no tag run exists, dispatch from the exact existing release tag.
gh workflow run sync-cnb.yml --repo codewhale-hq/CodeWhale --ref vX.Y.Z
```

修复标签时不要省略 `--ref`：在默认分支上 dispatch 同步的是 `main`，
不是 `refs/tags/vX.Y.Z`。之后，先证明该标签和它的 Linux x64 发布资产确实存在，
再引导用户去 CNB。

## 轮换 `CNB_GIT_TOKEN`

如果工作流开始报认证错误，而 token 已经过期：

1. 登录 `cnb.cool`，生成一个带 `repo`（push）权限的新个人访问 token。
2. 更新 `CNB_GIT_TOKEN` 仓库 secret：
   ```bash
   gh secret set CNB_GIT_TOKEN --repo codewhale-hq/CodeWhale
   ```
3. 在最近的提交上重新触发工作流：
   ```bash
   gh workflow run sync-cnb.yml --repo codewhale-hq/CodeWhale
   ```
4. 用 `gh run list --workflow=sync-cnb.yml` 确认运行成功。

## 二进制发布资产与 `codewhale update`

CNB 现在用纳入源码管理的 `.cnb.yml` 流水线，为 `v*` 标签构建 Linux x64 资产。
GitHub 仍是 macOS/Windows 的权威发布矩阵。

### 自动选择来源（Linux x64）

在 Linux x64 上，`codewhale update` 会在下载任何大文件之前先选好资产来源。
确定目标标签后，它会**同时**向 GitHub Releases 和 CNB release 请求该标签的
`codewhale-artifacts-sha256.txt`。哪个来源先应答、而且清单里列出了
`codewhale-linux-x64`，就采用哪个；晚到一方的应答会被丢弃。

这依赖三个性质：

- **清单就是探针。** 它只有几百字节，所以被封锁或响应缓慢的来源，大约在它自己
  连接失败的那一刻就已经出局。用户不必干等一个卡住的多兆字节资产下载，也
  没有任何超时替用户做选择。
- **清单和二进制来自同一个来源。** CNB 从打标签的那份源码自行构建产物（musl 静态，
  不是 GitHub 的 glibc 构建），所以两份清单描述的是不同的字节，不能互换。
  胜出的来源同时提供两者；校验和不匹配时更新直接失败，不会退回给输的一方。
- **选择过程不会改变安装哪个发布版本。** 标签仍然来自 GitHub 的稳定版或
  测试版查询，所以 `--beta` 的含义不变；探针只决定该标签的字节从哪里取。

`codewhale update` 和 `codewhale update --check` 都会把结果打印为
`Release source:` 行，安装后的摘要也会再重复一遍，因此事后可以查到某个二进制
文件来自哪个来源。

其他所有目标平台都只有一个权威来源：CNB 只发布 Linux x64，别的都不发，
所以 macOS、Windows、Android 和 Linux arm64 不会与 CNB 竞争；Linux riscv64
仍明确不支持。不过所有受支持的自更新路径都要求校验和：选中的来源必须为确切
平台上那个二进制文件发布一条有效的 `codewhale-artifacts-sha256.txt` 记录，
否则 `codewhale update` 会在下载该二进制文件之前停下。没有未经验证的安装回退。

设置 `CODEWHALE_RELEASE_BASE_URL`（或它的旧别名）或 `CODEWHALE_USE_CNB_MIRROR`
会完全关闭来源选择：显式指定了哪个来源，就用哪个，包括它自己的校验和清单；
其中 `CODEWHALE_RELEASE_BASE_URL` 优先于 `CODEWHALE_USE_CNB_MIRROR`。

### 手动路径

在 GitHub 被封锁的网络里，用户也可以显式选择来源：

- **从 CNB 镜像 `cargo install`：**
  ```bash
  cargo install --git https://cnb.cool/codewhale.net/codewhale --tag vX.Y.Z codewhale-cli --locked
  ```
  当前的 `codewhale` 二进制文件在进程内运行 TUI。Cargo 用户如果想要那个可选的
  短命令，可以在它旁边加一个 `codew` 符号链接；不需要另外安装 `codewhale-tui`。
  Linux 构建期依赖（Debian/Ubuntu 上的 `build-essential`、`pkg-config`、
  `libdbus-1-dev`）是必需项 —— 参见
  [INSTALL.md](./INSTALL.md#4-通过-cargo-安装任何-tier-1-rust-目标)。

- **CNB 发布资产**（Linux x64），前提是对应的 CNB 标签流水线已成功完成。
  从 `vX.Y.Z` 的 CNB release 下载 `codewhale-linux-x64`、`codew-linux-x64` 和
  `codewhale-artifacts-sha256.txt`，然后对照清单校验二进制文件。发布的
  `codewhale-tui-linux-x64` 文件是给旧客户端用的桥接文件，当前安装不需要它。
  在 Linux x64 和 OpenHarmony x64 上，npm 包装器会针对确切的包版本，同时探测
  该 CNB 校验和清单与 GitHub Releases，然后锁定第一个 HTTP 响应和清单都校验
  通过的来源；它不会等待缓慢的 GitHub 二进制下载。设 `CODEWHALE_USE_CNB_MIRROR=1`
  可以只走 CNB，设 `CODEWHALE_RELEASE_BASE_URL` 可以跳过这场竞争。其他平台
  必须用 GitHub，或用完整的 `CODEWHALE_RELEASE_BASE_URL` 镜像。

- **`CODEWHALE_RELEASE_BASE_URL`** 环境变量，前提是存在发布资产的 CDN 镜像。
  npm 包装器安装器和 `codewhale update` 会读取这个变量来重定向二进制下载。
  对 `codewhale update`，还要设置 `CODEWHALE_VERSION=X.Y.Z`，这样更新器不用
  联系 GitHub 也能标出镜像上的发布版本。指向的目录必须包含
  `codewhale-artifacts-sha256.txt` 和各平台二进制文件；格式与 GitHub Release
  的资产目录一致。更早的 `DEEPSEEK_TUI_*` 名字仍可作为兼容别名使用。

## 从 CNB 克隆

想要稳定的安装，请从下面这个地址克隆 `main` 或某个发布标签：

```bash
https://cnb.cool/codewhale.net/codewhale.git
```

镜像会收到 `main` 和发布标签。当 CNB 工作流或凭据不健康时，
GitHub 是回退方案。

腾讯云 Lighthouse 自建服务器的中文/英文操作步骤见
[`scripts/tencent-lighthouse/README.md`](../../scripts/tencent-lighthouse/README.md)。
CNB 部署按钮示例见 [`deploy/tencent-lighthouse/cnb/`](../../deploy/tencent-lighthouse/cnb/README.md)。只有把它们复制进
`.cnb.yml` 和 `.cnb/tag_deploy.yml` 之后才会生效，因为实际部署任务需要
Lighthouse 部署密钥、目标主机，以及明确的 CNB 配额/计费策略。
