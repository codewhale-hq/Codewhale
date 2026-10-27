# 构建与测试性能

> 英文原文：[BUILD_PERFORMANCE.md](../BUILD_PERFORMANCE.md)。
> 最后与英文同步日期（last synced with English revision）：2026-10-22（新增共享 Engine future 边界测量；下方保留历史数据）。

这篇文档记录实测数据：Codewhale 的构建和测试各要多久，为了让贡献者的开发循环
更快做了哪些改动，哪些又推迟了。数字来自一台机器（Apple Silicon，14 核，rustc 1.97.0，
Xcode 26.2 `ld-1230`），采集时还跑着另外四个 cargo 任务（1 分钟负载均值 10–27，
每个数字旁边都记了当时的负载），所以把这些数字当作前后对比的相对证据，不要当基准测试结果。

> 拆分计划：[TUI_DECONSTRUCTION.md](../design/TUI_DECONSTRUCTION.md) 记录了当前的
> 源码审计、目标 crate 地图和抽取顺序。下面的测量都是历史数据；B3 和“推迟”两处的候选清单
> 不是执行队列。

## 2026-10-22：共享 Engine future 边界

0.10.2 候选源码的 `crates/tui/src` 包含 1,040 个 Rust 文件、1,279,727 行物理源码
（含测试）。按测试名筛选仍会编译整个大型库测试目标。下面较早的测量属于旧源码和编译器。

在同一台 14 核 Apple Silicon 机器上，使用 Rust 1.99.0、相同增量缓存、特性和开发
profile，连续两次本地构建的结果为：

| 候选构建 | 墙钟时间 |
| --- | ---: |
| 共享 Engine 循环返回完整具体 future 类型 | 29 分 31 秒 |
| `Engine::run_owned` 返回装箱的 `Send` future | 2 分 24 秒 |

第一次构建的原生采样显示，一个编译器线程在单态化和 MIR 验证阶段递归解析 trait
约束。现在仅在现有共享循环边界擦除 future 类型，根智能体和子智能体仍使用同一个
事件循环；每次 Engine 调用增加一次分配。第二次构建还修正了三个现有测试断言并记录
Cargo `--timings`。这是本地热构建观察，不是隔离变量的基准，也不承诺冷构建、发布
构建或其他平台获得同样加速。两次构建及其聚焦检查期间，2,172 个记录的输入均未变化；
边界修改后的 58 项相关现有检查全部通过。最终发布验证仍是独立步骤。

本地编辑循环继续使用增量编译和现有行表调试信息设置。完成一组连贯修改后再编译，
输入不变时复用测试二进制执行相关筛选。只在本来就需要的验证构建中记录耗时。长期
工作仍是 [TUI 拆分](../design/TUI_DECONSTRUCTION.md)：将实现与现有测试一起迁出，
让每个子系统可以独立构建。

## 时间花在哪里（基线，commit 533c530b）

| 步骤 | 墙钟 | 说明 |
| --- | --- | --- |
| 冷启动 `cargo build -p codewhale-tui`（空 target） | 94 s（user 270 s） | 543 个单元；单是 `codewhale-tui` 就要 70 s，它是关键路径；紧随其后的单元是 `codewhale-config` 7.5 s、`jsonschema` 6.3 s、`codewhale-workflow` 5.6 s、`tokio` 5.4 s。负载 13。 |
| 冷启动 `cargo test -p codewhale-tui --lib --no-run`（空 target） | 148 s（user 347 s） | 装着 1.05 万个测试的单元测试二进制有 357 MB，会触发 macOS 链接器的 `__eh_frame > 16MB` compact-unwind 警告（无害）。负载 20。 |
| 改一行代码后的增量 `cargo build -p codewhale-tui` | 12.5 s | 负载 11。 |
| 改一行代码后的增量 `cargo test -p codewhale-tui --lib --no-run` | 19 s | 负载 11。 |
| 依赖已预热时 `cargo test --workspace --all-features --locked --no-run` | 155 s（user 366 s） | 61 个测试二进制。负载 6→11。 |
| 近乎冷启动的 `cargo check --workspace --all-targets --locked`（依赖已构建） | 82 s | 负载 19。 |
| 用 libtest 跑 tui 单元测试套件（`cargo test -p codewhale-tui --lib`） | 268 s | 取自发布门禁日志；10,531 个测试。负载约 10。 |
| 用 `cargo nextest run -p codewhale-tui --lib` 跑同一套件 | 96–108 s | 同样的测试，每个测试一个进程，所有核心跑满。负载 12–20。 |
| 用 `cargo nextest run --workspace --all-features` 跑整个工作区 | 353 s | 12,744 个测试，PTY 套件按 nextest 配置串行执行。负载 17。 |

这些数字背后的结构性事实：

- `crates/tui` 约有 74.6 万行 Rust（其中 60.9 万行非测试代码，13.7 万行内联测试
  分布在 488 个 `#[cfg(test)]` 模块里，另有 1.06 万个 `#[test]`/`#[tokio::test]`
  函数）。它按一个 crate 编译，所以这个 crate 的前端就是每次构建的关键路径，
  而每跑一次单元测试，都要带上 `cfg(test)` 把它重新编译一遍。
- 依赖已经精简过了（`reqwest` 用 rustls-no-provider，`image` 只要 png，
  `syntect` 用 default-fancy，`rmcp` 不带默认特性，`mimalloc` 不带默认特性）。
  `cargo tree -d` 只报常规重复（`toml` 0.8/1.1、`thiserror` 1/2、`strum` 0.27/0.28、
  `syn` 2/3、`sha2` 0.10/0.11），而且都来自第三方 crate，不是工作区的选择造成的。
  全局分配器默认是 mimalloc；`codewhale-tui`/`codewhale-cli` 上默认关闭的
  `rusty-alloc` cargo 特性会把它换成纯 Rust 重写的 `rusty_alloc`（#5872）。用
  `cargo build -p codewhale-cli --no-default-features --features rusty-alloc`
  （或 `-p codewhale-tui`）可以排除 mimalloc 及其 C 构建依赖。Cargo 特性是叠加的：
  只写 `--features rusty-alloc` 时，即使分配工作已经交给 Rust 分配器，默认的
  mimalloc 依赖依然在。这样做能去掉分配器的 C 构建路径；其他原生依赖可能仍然
  需要 C 工具链。两个分配器特性都不开时，用标准库的系统分配器。
- `[profile.dev] debug = "line-tables-only"` 已经设好了（#5246），macOS 上 Cargo
  也已经使用 `split-debuginfo = unpacked`。
- `target/debug` 会超过 50 GB，但那是不同特性集和不同 worktree 长期累积的结果；
  一次全新的测试构建约 7 GB。

## A0 回执（commit 533c530b + 封闭性修复；空 target 目录）

`CARGO_TARGET_DIR=/Volumes/VIXinSSD/CW/.tmp/compile-speed-baseline`，HTML 计时报告
存档在 `backups/compile-speed-evidence-20260815/`（a0-cold-lib-test-timing.html、
a0-incremental-lib-test-timing.html、a0-llvm-lines-top40.txt）。

| 回执 | 墙钟 | 负载（1 分钟） |
| --- | --- | --- |
| 冷启动 `cargo test -p codewhale-tui --lib --locked --no-run --timings` | 127 s（user 329 s） | 8.9 |
| `touch crates/runtime/src/elapsed.rs` + 同样的命令 | 21 s | 12.3 |
| `touch` + `cargo test -p codewhale-tui --lib --locked elapsed::`（日常循环） | 20 s（跑了 4 个测试） | 11.2 |
| 库测试二进制大小 | 357 MB（`codewhale_tui-<hash>`）；链接时报 `__eh_frame section too large (max 16MB)` compact-unwind 警告 | — |
| `touch` 之后增量执行 `cargo check -p codewhale-tui --lib --tests`（仅前端） | 14 s | 6.2 |
| `touch` 之后增量做完整库测试构建，条件相同 | 28 s | 6.2 |

冷启动计时报告里耗时最长的单元（共 605 个单元）：`codewhale-tui` 库测试
**106.0 s**，`codewhale-config` 7.9 s，`jsonschema` 5.8 s，`moxcms` 4.7 s，
`codewhale-protocol` 4.2 s，`tokio` 4.0 s，`rustls` 3.8 s，`schemaui` 3.4 s，
`rmcp` 3.4 s，`h2` 3.3 s，`jsonschema`（第二份副本）3.2 s，`codewhale-workflow`
3.2 s，`syn` 3.1 s，`rio-vt` 3.1 s，`regex-automata` 3.0 s。增量报告里只有一个
非零单元：`codewhale-tui` 库测试 20.8 s。所以日常要交的“税”就是 tui crate 自己，
大致一半花在前端（check --tests 14 s），一半花在代码生成加链接（合计 28 s）；
耗时不在依赖和链接器上。

`cargo llvm-lines -p codewhale-tui --lib`：**8,138,810 行、223,052 份副本**。
最大的单个函数是 `rust_i18n` 后端闭包（`_RUST_I18N_BACKEND::{closure#0}`，
311,782 行，单它一个就占了这个 crate 的 3.8 %——`i18n!` 宏把这 15 个语言包都编译进
一个 match），接着是 `run_event_loop` 2.7 万行、`Engine::run_turn` 2.6 万行、
`RuntimeThreadManager::monitor_turn` 1.6 万行，然后是 `Config`/`ProvidersConfig`/
`Settings` 的 serde `Deserialize` 展开（各 5–6 千行，每个 toml 反序列化器都有
好几份副本）。

### A0.1 依赖棘轮

`cargo metadata --locked` 数出 **690** 个包，`cargo deny check bans` 对重复的
`fancy-regex`、`jsonschema`、`jsonschema-regex`、`referencing` 报警，另外还报了
过期的 `jni`/`jni-sys`/`redox_syscall` 跳过项。原因：工作区把 `jsonschema` 的 pin
提到了 0.49，而 `schemaui` 0.12（含最新的 0.12.4）仍然要求 `^0.46`。把工作区
pin 改回 0.46 系列，第二套 jsonschema 依赖栈就消失了（**685** 个包；deny bans
和 advisories 都干净；`--locked` 能解析；codewhale-workflow-js 的 61 个测试和
tui 的 schema 测试都通过）。冷启动省下的就是那两个重复单元（单元时间约 9 s，
墙钟约 3 s）。

### A1 缓存拓扑（本机使用，未提交）

新建 worktree 的冷启动 `cargo test -p codewhale-tui --lib --locked --no-run`，
同一台机器，连续执行：

| 拓扑 | 墙钟 | CPU（user） | 说明 |
| --- | --- | --- | --- |
| 每个 worktree 各自全新 target（对照） | 127 s | 329 s | A0 |
| 一个共享的 `CARGO_TARGET_DIR`（已被另一个 worktree 预热） | 121 s | 188 s | 依赖复用；工作区里每个 crate 都要重编（按路径做键）；没观察到等锁；target 14 GB |
| 每个工作区各自的 `build.build-dir = ".../{workspace-path-hash}"` 加共享的预热 `sccache`（`CARGO_INCREMENTAL=0`） | 107 s | 161 s | sccache 命中率 73.6 %（337 个 Rust 依赖单元全命中；125 次未命中都是工作区 crate）；每个工作区 2.7 GB build 目录加 483 MB 缓存；*填充*缓存的那条命令本身耗时 108 s / 157 s CPU |

每种拓扑里，墙钟时间都由 tui crate 决定；这些拓扑换来的是 CPU 的节省（约 50 %），
而在多份 checkout 同时构建时，省 CPU 才是关键。推荐的用户级
`~/.cargo/config.toml`（两个根路径按需调整）：

```toml
[build]
# One build root for every checkout; each workspace gets its own subdir,
# so worktrees never wait on each other's target lock.
build-dir = "/path/to/cache/codewhale/build/{workspace-path-hash}"
# Optional: reuse dependency compilation across checkouts.
# rustc-wrapper = "sccache"
```

这次测量用的机器上，`sccache` 是用 `brew install sccache` 装的。

### 公共辅助脚本（A1/A5）——`dev-test.sh` 现在到底做什么

`scripts/dev-test.sh` 以前只把某个区域映射到 `cargo test -p`。它**没有**启用实测的
build-dir + sccache 拓扑，所以新 worktree 仍然要往 `./target` 里做一次冷编译。

`scripts/dev-cache.sh` 是可移植的按需启用辅助脚本。`scripts/dev-cargo.sh` 和
`scripts/dev-test.sh` 都 source 它。

| 类别 | 改了什么 | 它不是什么 |
| --- | --- | --- |
| **编译期** | `scripts/dev-test.sh` / `scripts/dev-cargo.sh` 会设置 `CARGO_BUILD_BUILD_DIR=$CODEWHALE_CACHE_ROOT/build/{workspace-path-hash}`，这样并发的 worktree 就不会抢同一个 Cargo 锁。残留的 `./target`（拆分 build-dir 后 Cargo 仍会往那里写 `CACHEDIR.TAG`）**不会**让隔离失效；如果你确实想留着 `./target`，用 `CODEWHALE_DEV_CACHE=local`。Cargo 低于 1.91 时，会退回按工作区设置 `CARGO_TARGET_DIR`。 | 不能缩小 rustc 编译单元。工作区 crate 照样重编。 |
| **编译期（sccache）** | 只有在增量已经关闭（`CARGO_INCREMENTAL=0` 或 `CODEWHALE_SCCACHE=1`）**且** `sccache` 在 `PATH` 上时，才设置 `RUSTC_WRAPPER=sccache` 和 `SCCACHE_DIR=$CODEWHALE_CACHE_ROOT/sccache/<rustc-commit>`。 | 日常增量循环里不启用。sccache 缓存不了增量单元；包装这类构建只会增加开销，命中率 0%。sccache 缺失时只打印一句回退说明，不算错误。 |
| **测试运行时** | 装了 `cargo-nextest` 时，`scripts/dev-test.sh` 用 `cargo nextest run`（`CODEWHALE_DEV_NEXTEST=0` 强制走 libtest）。二进制不变；每个测试一个进程。重试次数保持 0。`RUST_MIN_STACK=16MiB` 未设置时会导出。 | 不是编译优化。nextest 不跑 doctest；`cargo test --doc` 仍是单独的门禁。 |
| **易用性** | `--list` 和路径映射覆盖每个工作区 crate（`app-server`、`workflow-js` 等）。`scripts/dev-cache.sh --status` / `--self-check` 会打印当前拓扑。 | 不改变产品行为。 |

默认值里不含任何机器专属的绝对路径：

```sh
# Portable default:
#   ${XDG_CACHE_HOME:-$HOME/.cache}/codewhale
# Desk override, if you want the cache on a particular volume:
export CODEWHALE_CACHE_ROOT=/path/to/cache/codewhale

scripts/dev-cache.sh --self-check
scripts/dev-test.sh crates/command-contract/src/elapsed.rs
CARGO_INCREMENTAL=0 scripts/dev-cargo.sh test -p codewhale-config --lib --locked --no-run
```

封闭性脚本测试（不编译 rustc）：`sh scripts/dev-cache.test.sh`。
`scripts/dev-test.sh --self-check` 会报告辅助脚本解析出的缓存拓扑；它自己的脚本
测试 `scripts/dev-test.test.sh` 已在 `d64b9429b7` 中移除。

### 辅助脚本验证（2026-08-15，本 worktree）

记录于其他支线让出机器之后（负载 3.2–5.6）。rustc 1.97.0，cargo 1.97.0，
sccache 0.17.0。这次运行把 `CODEWHALE_CACHE_ROOT` 设成卷内的一个覆盖路径；
没有删任何缓存或 target。

对这个 worktree，Cargo 把 `{workspace-path-hash}` 展开成 `build/d4/96565f96fb3682`。
第一次隔离执行 `codewhale-config` 的 `--no-run` 时创建了一个占位的 `./target`
（`CACHEDIR.TAG`）；如果把它当成预热过的传统 target，下一条命令就会重新编译进
`./target`（8.65 s）。现在除非设置 `CODEWHALE_DEV_CACHE=local` 或 `0`，辅助脚本
都会保持隔离。

**编译期**（`scripts/dev-cargo.sh test … --locked --offline --no-run`）：

| 步骤 | 墙钟 | 说明 |
| --- | ---: | --- |
| 第一次隔离执行 `codewhale-config --lib --no-run` | 9.14 s（user 23.8 s） | 90 个单元写进带哈希的 build-dir |
| 预热后隔离执行同样的命令（修掉占位 target 问题之后） | 0.13 s | `Finished` 用 0.07 s |
| `touch crates/config/src/lib.rs` + 隔离 `--no-run` | 0.93 s | 只重编了 `codewhale-config` |
| 第一次隔离执行 `codewhale-tui --lib --no-run` | **121.5 s**（user 305 s） | 600 个单元；二进制 340 MB；A0 空 target 是 127 s / 329 s |
| `touch crates/runtime/src/elapsed.rs` + 隔离 `--no-run` | **18.15 s** | 日常编译循环；A0 是 21 s / 19 s |
| 在已经预热的树上执行 `CODEWHALE_SCCACHE=1` config `--no-run` | 5.21 s，随后 0.14 s | 设置了 wrapper 和 `SCCACHE_DIR=…/sccache/<rustc-commit>`；sccache 命中 0 次，因为只有工作区 crate 重编，而且 build-dir 没被清空 |

**测试运行时**：

| 步骤 | 墙钟 | 说明 |
| --- | ---: | --- |
| `scripts/dev-test.sh config`（nextest，557 个测试） | run 0.479 s / real 2.40 s | 含一次 0.85 s 的 profile 切换编译 |
| `CODEWHALE_DEV_NEXTEST=0 scripts/dev-test.sh config`（libtest） | body 0.11 s / real 0.27 s | 557 个小测试：这里每个测试一个进程反而更慢 |
| `scripts/dev-test.sh crates/runtime/src/elapsed.rs` | run 0.023 s / real 2.81 s | 4 个通过，10,516 个跳过；nextest 过滤器有效 |

268 s → 约 100 s 的 nextest 收益仍是更早那条 tui 单元测试套件回执。config 太小，
吃不到这个收益；对不加过滤的 crate/工作区运行来说，nextest 仍然是合适的默认选择。

**易用性：** 当时 `sh` 和 `dash` 都能通过 `dev-cache.test.sh`（22）和
`dev-test.test.sh`（27）；`dev-test.test.sh` 后来被移除（`d64b9429b7`）。
sccache 缺失会走回退。`--list` 覆盖每个工作区 crate。

### A2 CI 中的 nextest

`cargo test --workspace --all-features --locked --doc` 清点出 **21 个 crate 中
3 个通过 / 8 个忽略的 doctest**；CI 把它们保留为独立一步，与
`cargo nextest run --workspace --all-features --locked --profile ci` 并列。

### A3/A4（已测量，未采用）

前端和代码生成在 tui 单元里大致各占一半（增量 14 s / 14 s）；链接器只占其中
一小部分，而依赖在首次构建后就已经预热。所以 `[profile.dev.package."*"] opt-level = 1`
（配对结果见上）、`-Wl,-dead_strip` 和其他 `RUSTFLAGS` 都不进仓库（它们会作用到
发布的 profile 上）；macOS 上 `split-debuginfo` 已经是 `unpacked`。

## 内存峰值（为什么 OHOS/Windows 构建会出现两个约 4 GB 的 rustc 进程）

对本支线 target 目录下的每个 rustc 进程每秒采样一次 `ps -o rss`
（`backups/compile-speed-evidence-20260815/rss-sample.sh`、`mem-incremental.log`、
`mem-cold-cgu.log`）；每行一个 rustc。

| 单元 | 模式 | RSS 峰值 | 墙钟 | 负载 |
| --- | --- | --- | --- | --- |
| `codewhale-tui` lib（dev） | 增量，cgu 256 | 3.3 GB | 12–14 s | 6.3 |
| `codewhale-tui` lib test | 增量，cgu 256 | 6.0 GB | 21–28 s | 6.3 |
| `codewhale-tui` lib（dev） | 非增量（`CARGO_INCREMENTAL=0`），cgu 16 | **6.0 GB** | 78 s | 5.5 |
| `codewhale-tui` lib test | 非增量，cgu 16 | **8.0 GB** | 105 s | 5.5 |
| `codewhale-tui` lib test | 非增量，`codegen-units = 4` | 6.1 GB（−24 %） | 145 s（+38 %） | 5.5 |
| `codewhale-tui` lib test | 非增量，`codegen-units = 1` | 7.8 GB（−3 %） | 161 s（+53 %） | 5.5 |
| 次大的几个单元（codewhale-config、rmcp、tokio、schemaui、codewhale-workflow） | 任一模式 | 0.4–0.7 GB | — | — |

所以单跑一个 `cargo build -p codewhale-tui`，一个 rustc 就要约 6 GB；单元测试
构建要约 8 GB；而 `cargo test --workspace`（或 `--all-targets`）会把 tui crate 的
lib 和 lib test 两个单元与 CLI 一起排进并发队列，这正是社区成员在 Windows 上为
OHOS 交叉编译时报告的“两个各占约 4 GB 的 rustc 进程”（不同系统对 RSS 的统计
方式不同，形状是一样的）。内联测试模块给这个 crate 的峰值再加约 2 GB（+33 %）；
峰值和时间两头都被泛型膨胀推高（810 万行 LLVM、`rust_i18n` 闭包 31.2 万行、
config 结构体的 serde `Deserialize` 展开）。减少代码生成单元，峰值降得不多，
墙钟时间却涨得很多，默认不采用。

### 低内存构建配方（内存小于 16 GB 的机器、交叉构建）

```bash
# One rustc at a time: the tui lib and its unit-test build never overlap.
export CARGO_BUILD_JOBS=1            # or: cargo build -j1 ...
# Only the crate you are working on, only its library:
cargo build -p codewhale-tui
cargo test  -p codewhale-tui --lib -- <filter>
# Do NOT use --workspace/--all-targets on a small machine; run crates one
# at a time (scripts/dev-test.sh <area> picks the narrowest command).
# Optional, if 8 GB for the unit-test build is still too much (slower):
export CARGO_PROFILE_DEV_CODEGEN_UNITS=4   # ~6 GB peak, ~+40 % wall
# Cross-builds (e.g. OHOS) inherit the same numbers: add -j1 to the
# cargo/ohrs invocation and build the release profile, which peaks lower
# than the unit-test build because it carries no test modules.
```

### B1（剥离巨型测试）——已审计，但在约束下无法落地

最大的六个内联测试文件（tui/ui/tests.rs 2.21 万行 / 643 个测试，
tools/subagent/tests.rs 1.87 万 / 448，core/engine/tests.rs 1.78 万 / 358，
config/tests.rs 1.27 万 / 393，runtime_threads/tests.rs 9.3 千 / 141，
runtime_api/tests.rs 9.1 千 / 151）分别引用 crate 内部项 826 / 226 / 627 /
85 / 152 / 217 次（`crate::llm_client::mock`、
`crate::test_support::{EnvVarGuard, lock_test_env}`、
`core::engine::mock_engine_handle`、`crate::tui::app::App` 等），而 codewhale-tui
库总共只对外暴露四个 `pub` 项。它们全是白盒测试；除非把模块树公开，否则一个都
搬不到 `crates/tui/tests/` 去——而本支线接到的要求就是不要这么做。这个方案本可以
换来的收益是测试模块给 lib-test 单元加上的约 2 GB / 约 35 s；但要拿到它，先得做一个决定：
要么提供 `#[doc(hidden)] pub mod test_api`（为黑盒测试子集用到的那约 30 个符号
提供一个有意公开、不稳定的接口），要么接受单元测试套件留在 crate 内部。
这里只做记录，没有动手。

### B2 已落地（把叶子类型移出 codewhale-tui）

| 迁移 | 移出 tui 的行数 | 受影响的调用方 |
| --- | --- | --- |
| `core/tool_parser.rs` → `codewhale_core::tool_parser` | 662 | 0（re-export；集成 harness 改成 import 而不是 `#[path]`） |
| `tls.rs` → `codewhale_release::tls` | 21 | 0（crate 根处写 `use codewhale_release::tls;`） |
| `AppMode`（含纯实现）→ `codewhale_config::AppMode`；本地化的选择器字符串仍留在 `AppModeUi` | 约 150 | 3 个文件 import 这个 trait |
| `ApprovalMode`（含纯实现）→ `codewhale_execpolicy::ApprovalMode` | 约 60 | 0（re-export） |

合起来只占这个 crate 74.6 万行里的约 900 行：依赖方向理顺了，但 tui 单元的时间
和内存暂时看不出可测量的变化（上面那个 8.0 GB 的 lib-test 峰值是在这些迁移之后
采样的）。没有迁移的项以及原因：`ReasoningEffort`——它的实现要用到 TUI 自己
定义的 `ApiProvider`（`crates/tui/src/config.rs`），还要调用
`crate::config::is_exact_*_k3_route` / `crate::provider_lake`，所以必须先迁
`ApiProvider`（见下面 B3）；`approval/policy.rs`（风险分类）依赖 `command_safety`
和 `auto_review`；`hashing.rs` 是 15 行 sha2 包装，53 处调用点，单独搬出去对
编译时间没有价值；那些没参与编译的
`core/runtime_contract/{budget,context,ledger,manifest,profile,progress,retry,terminal,work}.rs`
没有任何调用方，也没有构建成本（`core/mod.rs` 把它们记为分阶段搭的脚手架，
TUI-DOG-017）——保持原样。

### B3 顺序

1. 把 `ApiProvider` 和精确路由辅助函数（`is_exact_*_route`）从
   `crates/tui/src/config.rs` 移进 codewhale-config，从而解开 `ReasoningEffort`
   的阻塞。**尚未开始，现在是关键路径**——见第 4 条。
2. **已落地。** `localization` + `locales/*.json` → `codewhale-localization`
   （31.2 万行的 `rust_i18n` 闭包离开了 tui 单元；只改语言包不再重编 TUI）。
3. **已落地。** `palette` → `codewhale-palette`；`command_safety` →
   `codewhale-execpolicy`（它本来就拥有 `ApprovalMode`，所以这次迁移是去掉一条
   依赖边，而不是新增）。
4. `client/`（各提供商（provider）的传输协议适配器）→ `codewhale-client`：**被第 1 条卡住，
   不只是排在它后面而已。** 排除文档注释和 `#[cfg(test)]` 块之后，`client` 仍有
   20 条生产代码里的 `crate::` 依赖边。其中三条很难处理：
   - `crate::config`——`Config`、`ProvidersConfig`、`ProviderConfig`、`TuiConfig`、
     `ApiProvider`、`RetryPolicy`、`validate_route`、`wire_model_for_provider_route`，
     还有约 130 个提供商 base-URL / model-id 常量。`crates/tui/src/config`
     本身有 2.97 万行，在生产代码里还依赖 `config_persistence`、`oauth`、
     `credentials`、`tui`、`fleet`、`goal_loop`、`sandbox`、`lsp` 等，所以它没法
     跟着 `client` 一起搬出去。
   - `crate::tools` ⇄ `client` 是真正的循环依赖：`client` 用
     `tools::schema_sanitize`、`tools::large_output_router` 和 `tools::truncate`，
     而 `tools/{spec,review,registry,rlm,verify,speech,fim,web_search,web/backend,subagent/advisor}.rs`
     用 `client::{CodewhaleClient, ProviderNativeSearchClient,
     ProviderNativeSearchRequest, SpeechSynthesisRequest,
     RemoteControlInferencePermit}`。
   - `crate::core` ⇄ `client` 形状相同：`client` 用
     `core::events::bounded_tool_projection_warning_names`，而
     `core/{engine,engine/preview,engine/dispatch,engine/turn_loop,engine/reviewer,protocol_parity}.rs`
     用 `client::{CodewhaleClient, PreparedOutboundRequest, canonical_json,
     parse_usage, is_reasoning_replay_placeholder, redact_url_for_display}`。
   所以第 1 条是全部前提：先把 `ApiProvider`、精确路由辅助函数和提供商常量
   移进 codewhale-config，然后再重新测量 `tools` 和 `core` 这两处的循环依赖。
5. **已落地，属于第 4 条中可做的那部分。** `models` + `model_catalog` →
   `codewhale-models`（1,835 行，140 个调用方文件）。它们在依赖主干上正好位于
   `client` 下面，彼此之间只有一条生产依赖边，对 TUI 其余部分则没有依赖边；
   而 `models` 本来就已经有一半是在给 `codewhale_core::{request, role}` 做 re-export 门面。
6. 然后是 `fleet/`、`tools/`、`core/engine`——切开的位置就是它们的测试本来就遵守的
   crate 边界，测量用 A0 表格。

## 每台机器只跑一个构建

`scripts/dev-cargo.sh` 和 `scripts/dev-test.sh` 在整个 Cargo 调用期间持有机器级
独占构建锁（`<cache root>/build.lock`，由 `scripts/build-lock.py` 实现）。Cargo
自带的锁是按 target 目录分的，所以两个智能体（agent）往不同的 target 目录构建时依然会并发
跑起来，把内存吃光。第二个构建会等待，并打印出锁在谁手里。设置
`CODEWHALE_BUILD_LOCK=0` 可以跳过锁，设置 `CODEWHALE_BUILD_LOCK_FILE` 可以指定
锁文件。同一台机器上如果跑着自托管 CI runner，而它的 `.env` 把
`CODEWHALE_BUILD_LOCK_FILE` 指向同一个路径，runner 也会参与这把锁：于是 macOS Test
任务从第一次测试构建一直持锁到任务结束。这把锁是建议性的：绕过这些脚本直接
启动 Cargo 不会取锁；在没有 `fcntl` 的平台上（Windows），会先打印一条警告，然后不加锁构建。

## 本支线改了什么

1. **`scripts/dev-cache.sh` / `scripts/dev-cargo.sh` 启用了从
   `scripts/dev-test.sh` 实测出来的隔离 build-dir 拓扑。** 除非停用辅助脚本，新
   worktree 不再往私有的冷 `./target` 里编译。sccache 按需启用，且受增量
   开关限制。脚本自检位于 `scripts/dev-cache.test.sh` 和
   `scripts/dev-test.sh --self-check`（`scripts/dev-test.test.sh` 已在
   `d64b9429b7` 中移除）。
2. **`cargo nextest` 已支持并有文档**（`.config/nextest.toml`）。测试二进制不变，
   每个测试一个进程，所以 tui 单元测试套件在这台机器上约 100 s 跑完，而不是
   约 270 s；慢测试或卡住的测试会直接报出名字，而不是把整个二进制拖住。PTY 二进制
   固定为一次只跑一个测试（它要操作伪终端和共享的 mock 服务器；现在它靠
   进程内互斥锁串行，而 nextest 的“每测试一进程”模型本来会绕过这把锁）；
   而会启动真实 `codewhale` 可执行文件的集成二进制，则限制最多四个测试并发，
   这样它的 30 s 启动预算在满负载机器上也能撑住。
   `cargo test --workspace --all-features --locked` 仍是权威门禁；nextest 是
   本地循环。
3. **有三个测试依赖执行顺序**——它们能通过，只是因为同一进程里另一个测试先
   安装了 rustls 加密提供商（crypto provider）：
   `codewhale-tui mcp::sse::endpoint_tests::message_before_endpoint_is_rejected_instead_of_buffered`、
   `codewhale-app-server tests::failed_config_set_keeps_the_stdio_bridge`，以及
   `tests::successful_config_set_still_invalidates_the_stdio_bridge`。现在每个
   测试自己安装提供商，跟生产代码启动时的做法完全一致。运行时代码没有任何改动。
4. **CONTRIBUTING.md 增加了一节 “Fast local loop”**：先讲
   `scripts/dev-cargo.sh` / `scripts/dev-test.sh`，然后是定向的 `-p` 过滤、
   nextest、每个 worktree 各自的隔离 build 目录，以及下面那些可选的加速项。
   共享 `CARGO_TARGET_DIR` 只在串行做主干工作时才推荐。

## 已测量但刻意未采用

- `[profile.dev.package."*"] opt-level = 1`（依赖只优化一次，工作区 crate 不动）。
  配对测量，连续执行，target 布局相同：从空 target 冷启动
  `cargo test -p codewhale-tui --lib --no-run` 从 148 s 变成 193 s
  （user 347 s → 778 s）；nextest 下的 tui 单元测试套件从 96 s 变成 81 s；
  增量重建没变化。测试快约 15 %，代价却是冷构建贵 2.2 倍，对第一次想构建 Codewhale
  的人来说不划算。如果贡献者平时主要是反复跑测试，可以自行在本地启用：把这段
  配置加到用户级 `~/.cargo/config.toml` 的 `[profile.dev.package."*"]` 段里。
- 在仓库的 `.cargo/config.toml` 里加额外的 `RUSTFLAGS`/链接器参数
  （`-no_deduplicate`、替代链接器）。Rustflags 会作用到每个 profile，可能改变
  发布的二进制；macOS 的系统链接器已经是 `ld-prime`，而实测的增量链接开销就在
  上面 12–19 s 的增量数字里。改为作为可选的本地加速项记录在文档里。

## 推迟：拆分 `codewhale-tui`

唯一还能改变这些数字形态的杠杆，是把 crate 拆开，这样改动一个叶子模块时，
不必重新类型检查 60 万行代码，也不必重新链接 357 MB 的测试二进制。按依赖顺序排列的
机械式候选（每一个目前都只依赖 `codewhale-config`/`codewhale-paths` 加上第三方
crate，而且调用方今天也只通过单一模块路径使用它们）：

| 候选 crate | 来自 | 为什么可以干净地切出去 | 需要 re-export 的调用方 |
| --- | --- | --- | --- |
| `codewhale-glyphs` | `crates/tui/src/tui/glyphs.rs` | 常量表加纯函数；不依赖 crate 内部任何东西。 | `crate::tui::glyphs` |
| `codewhale-i18n` | `crates/localization/src/lib.rs` + `crates/localization/locales/*.json` | `rust_i18n::i18n!` 宏会把全部 15 个语言包编译进承载它的那个 crate；搬出去之后，只改语言包不再重编 TUI。`MessageId` 是普通枚举。 | `crate::localization` |
| `codewhale-mcp-transport` | `crates/tui/src/mcp/{sse,stdio,external_import}.rs` | 已经在和 `codewhale-mcp` 通信；与 tui 的唯一耦合点是 reviewed-launch 绑定。 | `crate::mcp` |

拆分规则：只做纯搬迁，在原路径上加 `pub use` re-export，不改行为，一个 PR
一个 crate，每个 PR 都用上面的表来测量（冷构建、增量构建、增量测试构建、
`cargo test -p codewhale-tui --lib --no-run`）。预期收益：tui 前端耗时会随搬走的
行数一起下降；在这些模块的测试跟着搬走之前，测试二进制的链接时间不变。

## 可选加速项（非必需）

- `cargo install cargo-nextest`——见上文。
- `scripts/dev-cargo.sh` / `scripts/dev-test.sh`——每个 worktree 隔离的
  `build-dir`，外加可选的 sccache。用 `CODEWHALE_CACHE_ROOT` 覆盖根路径；
  不要把机器专属路径提交进仓库。
- 共享一个 `CARGO_TARGET_DIR` 只在串行做主干工作时才用（两个 cargo 落到同一个
  target 会 flock）。优先用上面的辅助脚本。
- `sccache` 作为 `RUSTC_WRAPPER` 时，在 `CARGO_INCREMENTAL=0` 下可以跨干净的
  checkout 缓存依赖编译，而且和 CI 的做法一致（`.github/workflows/ci.yml` 用
  `mozilla-actions/sccache-action` 加 `Swatinem/rust-cache`）。
