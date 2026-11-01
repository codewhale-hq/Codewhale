# 模型目录刷新

> 英文原文：[CATALOG_REFRESH.md](../CATALOG_REFRESH.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。
> 2026-10-06 补齐 reviewed 元数据的唯一 owner 与退役缓存边界。

Codewhale 如何让模型元数据保持最新：哪些部分已经自动更新，哪些要人工维护，
以及定时目录任务该做什么、不该做什么。

相关文档：[`PROVIDERS.md`](./PROVIDERS.md)、
RFC [`rfcs/UNIFIED_PROVIDER_LOGIN.md`](../rfcs/UNIFIED_PROVIDER_LOGIN.md)。

---

## 简短回答

| 问题 | 回答 |
|---|---|
| 用户想刷新模型，需要一个专门的模型吗？ | **不需要。** |
| Codewhale 会自动更新公开的模型目录吗？ | **会，在运行时更新**，数据来自 [Models.dev](https://models.dev/catalog.json)，TTL 约 24 小时。 |
| 离线内置种子会在 CI 里自动提交吗？ | **不会，但它是生成出来的。** 维护者运行 `seed lock` 和 `seed render` 后提 PR；手工改动会被 CI 拒绝（`seed render --check`）。 |
| 应该让 LLM 重写目录 JSON 吗？ | **不要。** 数据摄取是确定性的公开 JSON。LLM 能*审阅* PR，不能当事实来源。 |

---

## 分层（优先级由低到高）

共享的目录编译器按从低到高的顺序应用这些层：

```
0 bundled Models.dev
10 live Models.dev
12 Codewhale corrections (applied to layers 0 and 10 as they load)
15 verified cloud facts (optional, off by default)
20 exact provider-owned live roster
25 Codewhale account roster
30 config.toml
40 user overrides
policy DENY (final)
```

Codewhale 修正（corrections）存放在 `crates/config/assets/catalog_corrections.json`。
它们是云事实 `ModelFact` 形状的字段补丁，由同一套补丁代码应用到每一行
Models.dev 数据上——离线种子和实时刷新都一样——所以修正在每个安装上都成立。
当某条上游事实本身正确、但对某条 Codewhale 路由有误导性时，就用修正：
`pricing_withheld`（填写原因）会清除价格，让该路由把价格报告为未知，用于
目录无法区分的分级费率、套餐配额和计费表面；`max_output` 等其他字段用于修补
上限。每一条都带有原因。修正只修补已存在的行，从不新增或隐藏行，已签名的
云事实仍然可以覆盖它们。被修正的行保留自己的来源；由修正决定的价格，其价格
来源报告为 `CatalogSource::CodewhaleBundled`。不要为了压住某个值而手改离线
种子：实时刷新会替换种子里的那一行，这样的压制只在离线时有效。

云事实沿用现有的编译器和 provider lake，详见
[`CLOUD_FACTS.md`](../CLOUD_FACTS.md)。能力来源与价格来源相互独立：能力补丁
不能重新标注继承来的价格。云价格补丁会替换整个价格块；未指定的 token 类别
保持未知。

路由解析还会绑定提供商（provider）类型、已配置的身份和端点。提供商自己给出的新名单，
在其确切范围内就是权威。显式选定的模型依旧保持显式。Codex 的账户观测/原生
缓存和 Ollama 端点标签各自保留专属的可用性规则；公开目录里的某一行，
不能证明某个账户可以调用该模型。已安装的 Codex `account/read` 与 `model/list`
路径记录在 [`PROVIDERS.md`](./PROVIDERS.md) 中。

没有可用目录时，旧版补全列表仍是最后的兜底。内置种子和静态的
传输/计费规则仍由发布流程负责；刷新目录元数据既不会引入新的线协议方言，
也不会改变凭据/计费的归属。

关键代码：

| 组成部分 | 路径 | 作用 |
|---|---|---|
| 实时抓取 + 缓存 | `crates/engine/src/models_dev_live.rs` | 后台刷新、TTL、原子写入、新鲜度状态 |
| Schema / 解析 | `crates/config/src/models_dev.rs` | 不联网的 Models.dev JSON 结构 |
| 编译 + 来源 | `crates/config/src/catalog.rs` | 有序来源、独立的价格来源、policy deny、id 归一化 |
| provider lake 合并 | `crates/engine/src/provider_lake.rs` | 共享目录投影，提供商权威严格限定在路由范围内 |
| 离线种子资产 | `crates/config/assets/models_dev.bundled.json` | 仅作紧凑的离线兜底（`_meta.role` 已注明） |
| Codewhale 修正 | `crates/config/assets/catalog_corrections.json` | 应用到每一行 Models.dev 数据的字段补丁（`crates/config/src/catalog/corrections.rs`） |
| 校验脚本 | `scripts/catalog_models_dev.py` | 不含密钥的抓取/校验试运行（#4117） |
| 脚本测试 | `scripts/catalog_models_dev_test.py` | 离线结构/脱敏检查 |

---

## 已经自动更新的部分（运行时）

TUI/运行时启动时（且未被禁用）：

1. 若**磁盘缓存**存在，先用它预填模型选择器（哪怕缓存已过期）。
2. 缓存缺失或超过 **24 小时**时，在**后台抓取** Models.dev
   （15 秒超时、显式的 Codewhale user-agent、**不带任何凭据**）。
3. 成功时：原子写入 `~/.codewhale/catalog/models-dev-catalog.json`，
   并把结果行以 `CatalogSource::ModelsDevLive` 发布到 ProviderLake——第 10 层，
   不带端点指纹。Models.dev 是描述模型的公开目录，所以刷新出来的行，和它
   取代的第 0 层种子一视同仁，仍然可以被第 15 层修正。`CatalogSource::Live`
   保留给提供商自己、按凭据范围返回的 `/models` 应答，位于第 20 层。
4. 失败时：保留原有缓存，或退回**内置**种子。Models.dev 宕机
   绝不会让模型选择直接失败。

### 手动强制刷新

在 TUI 中：

```text
/model refresh
```

该命令派发 `AppAction::RefreshModelsDevCatalog`（异步执行，不会阻塞输入框）。
如果启用了受准入控制的云事实设置，它还会请求一次云刷新；硬禁用和信任密钥检查
依旧生效。实现位于 `crates/tui/src/commands/groups/core/core.rs` 和
`crates/engine/src/models_dev_live.rs`。

### 环境变量开关（测试 / dogfood / 离线）

| 变量 | 作用 |
|---|---|
| `CODEWHALE_MODELS_DEV_URL` | 覆盖基础 URL 或完整的 `*.json` 目录 URL |
| `CODEWHALE_MODELS_DEV_PATH` | 从本地文件加载目录；跳过网络 |
| `CODEWHALE_DISABLE_MODELS_DEV_FETCH` | 真值 → 永不访问网络（`1` / `true` / `yes` / `on`） |

默认值：

- 目录 URL：`https://models.dev/catalog.json`
- TTL：`24 * 60 * 60` 秒（`DEFAULT_MODELS_DEV_TTL_SECS`）
- 缓存文件名：Codewhale `catalog` 状态目录下的 `models-dev-catalog.json`

暴露给界面 / 状态标签的新鲜度取值：`bundled` | `live` | `stale` | `failed`。

---

## 不会自动更新的部分（仓库 / 发布）

在定时 PR 落地之前，以下内容仍需人工维护，或走发布流程：

| 表面 | 为什么会漂移 |
|---|---|
| `models_dev.bundled.json` | 离线种子，由经过审阅的 spec 和固定的 lock 生成（见下文）；通过 PR 刷新，而不是在运行时刷新 |
| `provider_descriptors.json` 默认模型 ID | 产品选择，不是纯目录导出；常量投影由生成器维护 |
| `catalog_corrections.json` 的 `reviewed` | 带精确来源回执的固有/选择器/传输兼容事实；生成到同一种子 |
| Rust 价格策略 | 厂商计费窗口、人民币换算及扣留/分级行为；纯参考观察保存在 reviewed 补充资料 |
| 新的 `ProviderKind` / 线协议方言 | 需要代码，光有 JSON 不够 |

运行时的实时刷新**不会**改写这些文件。最近安装、网络正常的用户仍能看到
Models.dev 的新行；离线的新克隆、CI 的封闭运行，以及没有缓存的首次启动，
仍然依赖种子。

---

## 维护者工具（不涉及 LLM）

### 校验 / 试运行抓取

```bash
# Fetch Models.dev + print counts (never writes disk)
python3 scripts/catalog_models_dev.py refresh

# Validate the committed offline seed still parses as Models.dev-shaped JSON
python3 scripts/catalog_models_dev.py snapshot --check \
  crates/config/assets/models_dev.bundled.json

# OpenRouter public /models listing (no API key), dry-run only
python3 scripts/catalog_models_dev.py refresh --provider openrouter \
  --sort newest --limit 100
```

脚本的设计约束（有意为之）：

- 只用公开端点——不带 `Authorization` 头，不用 API 密钥。
- 远程 JSON 里出现形似凭据的键，一律清除。
- `refresh` 和 `snapshot` 从不写入（`--write` / `--write-cache` 失败关闭）。
  唯一的写入路径是 `seed lock`，它只固定 spec 引用到的行，并投影到允许列表内的字段。

### 重新生成离线种子（#6396）

`crates/config/assets/models_dev.bundled.json` 是生成出来的。绝不要手改：
CI 会运行 `seed render --check`，出现任何差异都会失败。

| 文件 | 内容 | 由谁编辑 |
|---|---|---|
| `scripts/catalog/models_dev_seed.toml` | 要携带哪些上游行，以及它们的 Codewhale 提供商 id、线协议 id、默认值、规范关联，还有少数上游没有列出的人工整理行 | 人工，经审阅 |
| `scripts/catalog/models_dev_seed.lock.json` | 被引用的上游行（已按允许列表过滤），以及来源 URL、抓取时间和 sha256 | 只由 `seed lock` 写入 |
| `crates/config/assets/catalog_corrections.json` | 有意的压制：扣留的价格、收紧的上限、推理控制 | 人工，经审阅；在线时同样生效 |
| `crates/config/assets/catalog_corrections.json` 的 `reviewed` | 保留来源的固有事实、限定范围别名、补全引用、公开标签/源码支持日期、纯路由事实与参考价格 | 人工审查；保留来源回执 |
| `crates/config/assets/models_dev.bundled.json` | 包含 reviewed 补充资料的渲染种子 | 只由 `seed render` 写入 |

spec 只负责选择和映射；它不能写出与上游不一致的值（未知的键会被拒绝）。
如果某个上游值对某条 Codewhale 路由不对，就加一条修正。修正对种子和实时行都生效；
仅靠手改种子实现的压制，会在第一次实时刷新时消失。

1. `python3 scripts/catalog_models_dev.py seed lock --dry-run` 打印审阅报告：
   每一行的字段变化、上游现已认同的修正（删掉它们），以及未携带的上游模型。
   当某个被引用的行从上游消失，或某个人工整理的行已出现在上游（改成派生行）时，
   它会失败。
2. 按报告要求编辑 spec 或修正。
3. `python3 scripts/catalog_models_dev.py seed lock` 写入 lock。
4. `python3 scripts/catalog_models_dev.py seed render` 写入种子。
5. 检查默认线协议 ID 仍与 `DEFAULT_*_MODEL` 一致，运行目录测试，
   然后提 PR，并把报告放进 PR 正文。

可选：用一个便宜模型在 **PR 正文中**总结“新增 / 移除 / 默认风险”——
但绝不让它当 JSON 的作者。

---

## 推荐的定时任务（尚未发布）

目标：让**仓库内的离线种子**不至于腐烂，同时不给 CI 改写密钥的权力，
也不放任 LLM 自行改写。

```text
cron (daily or weekly)
  → fetch Models.dev (public, no keys)
  → validate shape + scrub
  → compare against crates/config/assets/models_dev.bundled.json
     (and optionally report new ids vs provider defaults)
  → if material change: open PR
       title: chore(catalog): refresh Models.dev offline seed
  → optional: include an agent-written, human-readable diff summary in the PR body
```

这样的任务会运行 `seed lock` 和 `seed render` 并提 PR。用默认的
`GITHUB_TOKEN` 打开的 PR 不会触发 CI，所以它需要 bot token 或 GitHub App，
这得由维护者来配置。

### 自动化的范围内

- 以确定性方式从 Models.dev 摄取目录
- 不含密钥的 PR diff
- 漂移报告（新增模型 id、缺失的默认值、价格是否存在）

### 自动化的范围外

- Claude Pro/Max / 订阅制 OAuth 的“模型发现”（不是受支持的第三方路径；
  Anthropic 期望第三方工具使用 API 密钥）
- 未经审阅就让 LLM 改写 `models.rs` / `provider.rs`
- 强推 `main`，或在默认分支上悄悄改写资产
- 把 Models.dev 当作 OAuth 范围路由的唯一事实来源（Codex 名单仍特殊处理）

### 建议的工作流位置

`CodeWhale/.github/workflows/catalog-refresh.yml`（或类似名字），复用
`scripts/catalog_models_dev.py`，但前提是先做一次有意为之的**写安全**扩展，
让它只在 CI 里运行、用 bot token 创建 PR；如果写入会落进仓库，
即便走 `workflow_dispatch`，也仍需审阅。

如今的 nightly（`/.github/workflows/nightly.yml`）只构建发布产物，
**不**刷新目录。

---

## 需要一个“专门用来更新模型的模型”吗？

**核心流程不需要。**

| 任务 | 合适的工具 |
|---|---|
| 让用户看到的已知模型、窗口和价格跟着 Models.dev 保持新鲜 | 运行时实时抓取（已发布） |
| 让离线种子和发布资产在 git 中保持最新 | 定时 CI → PR（待建设） |
| 决定是否上调产品默认模型 | 人，或智能体（agent）在 PR 上*审阅* |
| 接入全新的提供商类型 / 方言 | 人工 PR + 测试 |

LLM 至多是目录 PR 的可选**审阅者**，不适合当目录 JSON 的**事实来源**。

---

## 认证说明（Claude / Anthropic）

刷新 Anthropic 的模型**目录**不需要 Claude Pro/Max OAuth。Models.dev 是公开的。
Codewhale 的 Anthropic 路由在推理时仍**基于 API 密钥**（`ANTHROPIC_API_KEY`）。
不要把目录自动化和订阅制 OAuth 或 Claude Code 的身份请求头绑在一起。

---

## 运维速查清单

- [ ] 正在运行的安装：确认网络未被阻断；厂商大发布后可选执行
      `/model refresh`。
- [ ] 离线 / CI 封闭环境：设置 `CODEWHALE_DISABLE_MODELS_DEV_FETCH=1`，
      或把 `CODEWHALE_MODELS_DEV_PATH` 指向测试夹具。
- [ ] 发布前：运行 `seed lock --dry-run`，看离线种子与 Models.dev 漂移了多少；
      如有必要，通过 PR 重新 lock。扫一眼 `PROVIDERS.md` 里已知的漂移。
- [ ] Models.dev 新增了某个你默认发布的主要系列之后：把种子 PR 和
      默认模型决策分开考虑。
- [ ] 刷新 Models.dev 时，绝不把 API 密钥粘进目录资产或自动化脚本的环境变量里。

---

## 议题 / 设计锚点

- 实时 Models.dev 层：#4187
- 内置种子降级（不再与实时数据争夺权威）：#4188
- 目录自动化脚本（校验 / 试运行）：#4117
- 生成的离线种子与运行时修正：#6396
- 更细的元数据清单与漂移列表：`codewhale-ops` 仓库

### 已退役的无范围元数据读取器

旧 models crate 缓存读取器及其独立内置资产、TUI 专用模型注册表已经退役。
已有旧缓存文件保留，但不作为提供商或公开标签权威导入。`config::catalog` 拥有
不可变的编译固有事实投影；现有 Engine provider lake 与限定范围目录缓存继续拥有
实时、账号及配置事实。不同端点上相同的线协议名称不构成规范关联、公开标签或
无范围价格。内置新鲜度使用真实种子 lock 的抓取时间，每次查询重新读取当前时钟。

兼容补全列表引用提供商描述符默认值与目录组，不再复制它们。Kimi 生成默认值与
直连/会员路由限制保持不同。未知能力及名称后缀推断的预算明确未验证。
网站模型日期表示源码支持日期，旧已证实日期保留在同一 reviewed owner 中；
它们不声明厂商发布日期或当前 API 可用性。
