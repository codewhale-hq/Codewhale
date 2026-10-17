# 本地化矩阵

> 英文原文：[LOCALIZATION.md](../LOCALIZATION.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

凡 Codewhale 已发布、正在构建、已排期或明确搁置的区域设置（locale），都以本文档为准。

> **范围说明（2026-07-12）：** 本矩阵覆盖三个层面——TUI 语言包
> （`crates/localization/locales/`）、已翻译的 README（仓库根目录），以及网站（`web/`）。
> 三者发布节奏不同，所以同一个区域设置可能在某个层面已**发布**、在另一个层面还是
> **已排期**；下面每个层面各有一张表，每张表只讲自己层面的事实。网站注册表是
> `web/lib/i18n/config.ts`（`ALL_LOCALES`）：区域设置切换器和路由生成都从它派生。
>
> 文档翻译**不属于**区域设置层面：它们放在 `docs/zh_hans/` 和 `docs/id/` 下，
> 状态记在 `docs/zh_hans/README.md` 和 issue #5482 里，不在本矩阵中。

面向客户的文案还要遵守 [Codewhale 语气与终端章程](./VOICE.md)；
命令、按键名和字形仍由代码决定，本地化只动周围的文字。
中文译名以 [中文术语表](./GLOSSARY.md) 为准；同一英文术语在一篇文档内只用一个中文译法。

最后更新：2026-08-18（docs/zh_hans/ 重构；按 #5482，文档翻译状态在本矩阵之外跟踪）。
权威 README：`README.md`（英文，#3087 之后）。

## 状态说明

| 状态 | 含义 |
|--------|---------|
| **shipped** | 已发布到 codewhale.net 和/或作为独立 README 发布，或 TUI 语言包与 `en.json` 完全对等 |
| **partial** | 已发布但有意不完整；缺的范围回退到英文，且 partial 状态可见 |
| **planned** | 明确排进下一波 |
| **deferred** | 承认需要但尚未排期；需要布局 QA、桥接支持或社区推动者 |

---

## TUI 语言包

`crates/localization/locales/` 下的 TUI 语言包是仓库里翻译量最大的地方。
`en.json` 是参照标准；语言包要和它的原始键（key）完全对等才算**完整**，
这一点由 `scripts/check-tui-locale-parity.py`（CI）和 `crates/localization/src/lib.rs`
里的对等测试把关。编写约定见 `crates/localization/locales/AGENTS.md`。

| 区域设置 | 文件 | 与 `en.json` 的键 | 状态 | 备注 |
|--------|------|--------------------------|--------|-------|
| 英语 | `en.json` | 全部 | **shipped** | 参照语言包。 |
| 日语 | `ja.json` | 全部 | **shipped** | 完整。 |
| 简体中文 | `zh-Hans.json` | 全部 | **shipped** | 完整。 |
| 繁体中文 | `zh-Hant.json` | 全部 | **shipped** | 完整（#5143）。等待母语者审核。 |
| 巴西葡萄牙语 | `pt-BR.json` | 全部 | **shipped** | 完整。 |
| 拉美西班牙语 | `es-419.json` | 全部 | **shipped** | 完整。注意网站跟踪的是 `es`——已发布的 TUI 语言包是拉美西班牙语，不是 `es-ES`。 |
| 越南语 | `vi.json` | 全部 | **shipped** | 完整。 |
| 韩语 | `ko.json` | 全部 | **shipped** | 完整。 |
| 加泰罗尼亚语 | `ca.json` | 全部 | **shipped** | 完整（#4749/#4788）。等待母语者审核。 |
| 德语 | `de.json` | 全部 | **shipped** | 完整（#4788）。等待母语者审核。 |
| 法语 | `fr.json` | 全部 | **shipped** | 完整（#4788）。等待母语者审核。 |
| 印尼语 | `id.json` | 全部 | **shipped** | 完整（#4789）。等待母语者审核。 |
| 印地语 | `hi.json` | 全部 | **shipped** | 完整（#4790）。天城文排版验证（Devanagari shaping spike，已在 `7242381022` 移出本仓库）只给出代码层面的保证；终端视觉 QA 和母语者审核仍未完成。 |
| 俄语 | `ru.json` | 全部 | **shipped** | 完整（#3092）。西里尔字母夹具防止混入其他语言的文案。等待母语者审核。 |
| 乌克兰语 | `uk.json` | 全部 | **shipped** | 完整（#4791）。西里尔字母夹具确保它与俄语区分开（不含 ы/э/ъ；含 і/ї/є/ґ）。等待母语者审核。 |

## 网站区域设置

网站的路由、切换器、站点地图和 hreflang 都来自 `web/lib/i18n/config.ts` 里的
`ALL_LOCALES`——只有这一个注册表，不再另建一套分类。**partial** 的区域设置照样有路由，
也能在切换器里选中，只是带一个可见的 `(partial)` 标记；它们的词典
（`web/lib/i18n/dictionaries/<code>/`）覆盖共用的界面框架
（masthead、nav、mobile menu、theme toggle、live ticker、footer、switcher）和首页。
这些词典由 `npm run check:locales` 与 `web/lib/i18n/dictionaries.test.ts` 要求
与英文参照键完全对等。这个范围之外的一切都渲染英文页面文案——这是有意设计的回退，
屏幕上绝不会出现词典的键名。

**从 #4934（v0.9.4）起，每个有路由的区域设置都走同一条词典路径，中文也不例外。**
`web/app/[locale]/page.tsx`、`web/components/nav.tsx` 和 `web/components/footer.tsx`
不再保留 `isZh` / `foreign` 的文案分支：它们改为读取 `getHome(locale)` 和
`getChrome(locale)`。`web/lib/i18n/dictionaries/zh/` 现已存在（以前是内联 TSX），
导航和页脚的链接集统一在 `web/lib/i18n/links.ts` 里生成一次，
这样每个区域设置得到的路由形状完全一致。

**网站/文档翻译流水线（General Translation CLI，2026-08-28）。** 运行时仍然是上面的
词典——不要在它旁边再加 `gt-next`。`web/gt-catalog/[locale].json` 是本地的
JSON 交换格式（先做 `en` + 已上线的 `zh`）。`npm run i18n:gt -- export` 从词典
导出 catalog；`check`（挂在 `check:locales` 上）要求两者一致；`import` 只把审阅过的
JSON 写回网站的词典 TS。`translate` 一律失败关闭，除非环境里设好 BYOK 的
`GT_API_KEY` 和 `GT_PROJECT_ID`——这两个值绝不能提交，也绝不要把这份配置指向
`crates/localization/locales`，更不要用它包裹模型补全。`gt generate` 不使用：
它是框架的 JSX 扫描器，不是 JSON catalog 工具。参照形状：**`ChromeDict` 52 个键、
`HomeDict` 62 个键。** 双语次级导航标签、masthead 的印章与期号行、ticker 的 live 标签，
以及按区域设置取值的 `Intl` 日期标记，都是词典的值——
不会有哪个区域设置意外渲染出另一种语言的文字。

| 区域设置 | 代码 | 状态 | 备注 |
|--------|------|--------|-------|
| 英语 | `en` | **shipped** | 源文案，也是词典的参照形状。每个页面都有 EN 路由。 |
| 简体中文 | `zh` | **shipped** | 在所有一等页面上与 EN 完全对等。从 #4934 起，界面框架和首页由词典支撑（`dictionaries/zh/`）；其余页面正文仍是内联的 `{ en, zh }` 内容模块。 |
| 日语 | `ja` | **partial** | #3091。界面框架和首页通过词典本地化；其他页面正文和元数据回退到英文。 |
| 越南语 | `vi` | **partial** | #3091。范围同日语。 |
| 韩语 | `ko` | **partial** | #3093。范围同日语。 |
| 俄语 | `ru` | **partial** | #3092。范围同日语。 |
| 乌克兰语 | `uk` | **partial** | #4791——与俄语一起发布，范围相同。 |
| 西班牙语 | `es` | **partial** | #3093。范围同日语。 |
| 巴西葡萄牙语 | `pt-BR` | **partial** | #3093。范围同日语。 |
| 法语 | `fr` | **planned** | #4788——TUI 语言包已在 v0.9.2 发布；网站排在下一波。 |
| 德语 | `de` | **planned** | #4788——TUI 语言包已在 v0.9.2 发布；网站排在下一波。 |
| 加泰罗尼亚语 | `ca` | **planned** | #4749/#4788——TUI 语言包已在 v0.9.2 发布；网站排在下一波。 |
| 印尼语 | `id` | **partial** | #4789。范围同日语。 |
| 印地语 | `hi` | **planned** | #4790——TUI 语言包已在 v0.9.2 发布；网站排在下一波。 |
| 阿拉伯语 | `ar` | **deferred** | RTL 候选。在布局与排版的 QA 到位之前搁置（双向文本、镜像界面框架、数字格式）。 |

每个 partial 区域设置都带完整的 52/62 键集（见 `npm run check:locales`）；
界面框架和首页是真正翻译过的，不是英文直接透传——非英文语言包里只要有成句的英文取值，
`dictionaries.test.ts` 就会失败。v0.9.4 新增的字符串，机翻标准与各自语言包其余部分
相同，并且**等待母语者审核**——这一点与上面的 TUI 语言包一致。

partial 区域设置在网站上还差的范围（下一波）：首页之外的逐页正文，以及
`generateMetadata` 的标题和描述；`web/lib/content/` 下 `{ en, zh }` 的共享内容模块；
`web/components/thinking-trace.tsx` 里的 TerminalPlayer 场景片段；还有
`web/components/feed-card.tsx` 里的 `KIND_LABEL` 键值对。词典层、路由、hreflang
和切换器都已经覆盖它们，所以补一个页面只是改词典，不用动管道。
剩下的那些英文，正是 `(partial)` 标记如实说明的部分。

## README 区域设置

| 区域设置 | 文件 | 状态 | 对等检查 |
|--------|------|--------|-------------|
| 英语 | `README.md` | **shipped** | 权威源文件 |
| 简体中文 | `README.zh-CN.md` | **shipped** | `scripts/check-readme-translations.py`（同步戳 + 代码围栏 + URL + 章节） |
| 日语 | `README.ja-JP.md` | **shipped** | 同上 |
| 越南语 | `README.vi.md` | **shipped** | 同上 |
| 韩语 | `README.ko-KR.md` | **shipped** | 同上 |
| 拉美西班牙语 | `README.es-419.md` | **shipped** | 同上 |
| 巴西葡萄牙语 | `README.pt-BR.md` | **shipped** | 同上 |
| 俄语 | `README.ru.md` | **shipped** | 同上（#3092）。等待母语者审核。 |
| 乌克兰语 | `README.uk.md` | **shipped** | 同上（#4791）。等待母语者审核。 |
| 印尼语 | `README.id.md` | **shipped** | 同上（#4789）。等待母语者审核。 |
| 法语 | `README.fr.md` | **shipped** | 同上。等待母语者审核。 |
| 德语 | `README.de.md` | **shipped** | 同上。等待母语者审核。 |
| 繁体中文 | `README.zh-TW.md` | **shipped** | 同上。等待母语者审核。 |
| 印地语 | `README.hi.md` | **shipped** | 同上。等待母语者审核。 |
| 土耳其语 | `README.tr.md` | **shipped** | 同上。等待母语者审核。 |
| 意大利语 | `README.it.md` | **shipped** | 同上。等待母语者审核。 |
| 波兰语 | `README.pl.md` | **shipped** | 同上。等待母语者审核。 |
| 阿拉伯语 | `README.ar.md` | **shipped** | 同上。等待母语者审核。只用 Markdown；不加 HTML `dir` 属性。 |
| 加泰罗尼亚语 | `README.ca.md` | **shipped** | 同上。等待母语者审核。 |

## 漂移检查

| 检查项 | 工具 | 状态 |
|-------|------|--------|
| 完整语言包与 `en.json` 的键对等 | `scripts/check-tui-locale-parity.py` + `crates/localization/src/lib.rs` 里的对等测试 | **Shipped**（CI Lint 任务） |
| README 译文与 `README.md` 保持同步 | `scripts/check-readme-translations.py` | **Shipped**（CI Lint 任务） |
| README 各区域设置链接对称 | `scripts/check-readme-locales.sh` | **Shipped**（CI Lint 任务） |
| 网站词典覆盖除 `en` 参照之外每个有路由的区域设置 | `npm run check:locales` + `web/lib/i18n/dictionaries.test.ts` | **Shipped**（#3091，#4934 扩展到 `zh`） |
| 非英文的网站词典里不残留未标记的英文文案 | `web/lib/i18n/dictionaries.test.ts` 里的 `leaves no unmarked English prose in any non-English dictionary` | **Shipped**（#4934） |
| 每个有路由的区域设置，导航/页脚路由在语言互换时保持对等 | `web/lib/docs-ia.test.ts`，跑在 `web/lib/i18n/links.ts` 上 | **Shipped**（#4934） |
| `Accept-Language` 确定性地路由到每个有路由的区域设置 | `web/lib/i18n/detect.test.ts`（中间件委托给 `lib/i18n/detect.ts`） | **Shipped**（#3091） |
| 区域设置选择器列出每个有路由的区域设置，并带 partial 标记 | `web/lib/i18n/config.test.ts`（切换器和路由都派生自同一个注册表） | **Shipped**（#3091） |
| hreflang 备用链接覆盖每个有路由的区域设置 | `web/lib/page-meta.test.ts` | **Shipped**（#3091） |
| 西里尔语言包保持字母纯净（不混入其他语言文案，ru≠uk） | `crates/localization/src/lib.rs` 里的 `cyrillic_packs_have_script_purity_and_no_mixed_language_fixtures` + `dictionaries.test.ts` | **Shipped**（#3092/#4791） |
| 天城文在 40/60/80 列宽下按字素安全截断/折行 | `crates/localization/src/lib.rs` 里的 `truncate_to_width_never_splits_devanagari_clusters` + 宽度夹具 | **Shipped**（#4790） |
| 新增 UI 区域设置不会改变模型可见的提示词字节 | `crates/tui/src/prompts.rs` 里的 `v092_locales_add_no_prompt_bookends_so_prompt_bytes_stay_stable` | **Shipped**（缓存稳定性契约） |
| 已发布的区域设置都不会渲染出缺失消息标记 | `crates/localization/src/lib.rs` 里的 `no_shipped_locale_renders_a_missing_message_marker` | **Shipped** |

## 如何新增一个区域设置

只有下面三个层面都处理完——要么发布该区域设置，要么在本矩阵里给它一行明确的
`planned`/`partial`/`deferred`——一个区域设置才算“加好了”。

### 1. TUI 语言包

1. 新建 `crates/localization/locales/<tag>.json`，把 `en.json` 里的每个键都包含进去，
   并遵守 `crates/localization/locales/AGENTS.md`（占位符保持字面量；按语言包惯例，
   产品术语保留英文；有意写的前导/尾随空格要保留）。
2. 在 `crates/localization/src/lib.rs` 里加上 `Locale` 枚举变体，以及它的
   `tag`/`translation_target_name`/`parse_locale`/`shipped`/`shipped_complete` 分支，
   并在测试模块里加上 `include_str!` 分支。
3. 还要把仍然手写枚举区域设置的地方都接上。`locale` 设置项是
   `crates/config/src/settings_schema.rs` 里的一行普通字符串，由
   `normalize_configured_locale` 校验（它复用第 2 步的 `parse_locale`）；`/config`
   的取值列表（`crates/tui/src/tui/views/mod.rs` 里的 `config_choice_values`）和提示文字
   （`configured_locale_values`）都从 `Locale::shipped()` 派生，所以这两处不用改。
   检查设置向导（`crates/tui/src/tui/setup/mod.rs`）以及
   `crates/tui/src/commands/groups/config/config.rs` 里的 `locale_display`
   是否需要更新显式区域设置映射。还要检查
   `crates/tui/src/commands/groups/core/links.rs` 里的 `public_site_locale_segment`：
   可移植的 `/links` 命令匹配的是区域设置 tag，而不是宿主的 `Locale` 枚举。
   未列出的 tag 会回退到英文网站页面，因此编译器不会在这里提醒新增的区域设置。
   当网站提供该语言版本时，添加显式 tag 映射，并验证 `/links` 选择了正确的文档和社区 URL。
   还要给引导流程的语言选择器
   （`crates/tui/src/tui/onboarding/language.rs`）加一条 `LANGUAGE_OPTIONS` 条目；
   不加的话，它的 `picker_offers_every_shipped_locale` 测试会失败。有几个
   “不泄漏英文”的测试（例如 `status_picker.rs` 和 `tool_card.rs` 里的）显式列出了
   区域设置；语言包补齐后，记得把新 tag 加进去。
4. 运行 `python3 scripts/check-tui-locale-parity.py` 和
   `cargo test -p codewhale-tui localization`。
5. 如果语言包必须带着缺口发布，就声明它是 partial：不要放进 `shipped_complete()`，
   在 `is_partial_pack()` 里标出，并把 tag 连同跟踪 issue 加进
   `scripts/check-tui-locale-parity.py` 的 `PARTIAL_PACKS`。目前没有任何语言包是 partial——
   `PARTIAL_PACKS` 是空的，`is_partial_pack()` 对每个已发布区域设置都返回 false——
   也就是说，要重新打开英文回退这条路，只有新增条目这一个办法。

### 2. README

1. 把 `README.md` 翻成 `README.<tag>.md`，保留结构、命令，以及 #3087 确立的史实。
2. 在 `README.md` 的语言行和其他已翻译的 README 里与它互链。
3. 按 `scripts/check-readme-translations.py` 的规定重新盖同步戳，然后运行
   `python3 scripts/check-readme-translations.py` 和
   `bash scripts/check-readme-locales.sh`。

### 3. 网站

1. 在 `web/lib/i18n/config.ts` 的 `ALL_LOCALES` 里新增或切换区域设置条目——
   切换器、路由、中间件、站点地图和 hreflang 都从它派生，所以不必单独改切换器。
   如果某个区域设置先只发布到“界面框架 + 首页”的词典范围，还没做到全页面对等，
   就用 `partial` 状态。
2. 按照英文参照的形状（`dictionaries/en/`）新建
   `web/lib/i18n/dictionaries/<code>/chrome.ts` 和 `home.ts`。
3. 基础 tag 不需要改中间件的语言检测；地区变体和 base→variant 映射都在
   `web/lib/i18n/detect.ts` 里。
4. 运行 `cd web && npm run check:locales && npm test && npm run build`。

### 4. 矩阵

更新上面 TUI、README 和网站三张表——每个层面一行，按层面各自标注状态。

## 评估

### 加利西亚语（`gl`）和巴斯克语（`eu`）——2026-07-25，见 #4749

这份评估是连同加泰罗尼亚语语言包（#4749 / #4788）一起做的；那份提案问的是：
加利西亚语和巴斯克语算不算“价值相当的欧洲新增项”，值不值得放在同一波发布。

**结论：两者都搁置。** 理由：

- #4788 支持加泰罗尼亚语的理由很具体：它“有着格外深厚的软件本地化传统和活跃的
  志愿者社区”——这是审校人力上的论据，不是市场规模上的。这个论据搬不过来：
  加利西亚语和巴斯克语的本地化社区明显更小，给它们做语言包，
  发出去也没有现实路径找到母语者来审核。
- 加利西亚语使用者已经有可用的降级方案：已发布的 `es-419` 语言包
  （`pt-BR` 在词汇上也很接近）。巴斯克语是孤立语言，没有邻近语言可以顶上——
  三者之中，它的逐字符串审校成本最高，机翻的巴斯克语也最不可信。
- 也不存在天然的“一起发布”分组：v0.9.2 那一波打包的是验收标准相同的区域设置
  （拉丁字母的 fr/de/ca/id、西里尔的 uk、天城文的 hi）。gl/eu 唯一的共同点是
  审校人力这条约束，而两者都没跨过去。

**支撑这个决定的成本与需求证据：** 一个完整的 TUI 语言包是 1,299 个键
（约 8–12k 词），再加上一项长期义务：英文一改，就要同步重译每个受影响的字符串——
对等门禁会把静默漂移变成 CI 失败，所以没人维护的语言包比没有更糟。没有任何社区成员
提过对 gl 或 eu 的需求（没有 issue、没有 PR、也没有人提交翻译），而 gl/eu 的基础 tag
已经能干净地通过 `web/middleware.ts` 路由——只等推动者出现。我们不会发布拿不到
母语审核的语言包，也不会为没发布的语言包做宣传。

等这两种语言出现母语者推动者，或者 v0.9.2 之后加泰罗尼亚语的采用情况显示出需求，
再重新评估。真到那时，两个基础 tag（`gl`、`eu`）都能通过 `web/middleware.ts` 路由，
中间件不用改。

## 相关 issue

- #3091 —— 网站对齐 JA + VI 这两个 README 区域设置
- #3092 —— 俄语 README 与网站本地化
- #3093 —— 韩语、西班牙语、巴西葡萄牙语列为下一波区域设置
- #3087 —— 品牌更名后刷新 README 源文案
- #4057 —— 把 `zh-Hant` 划为带英文回退的 partial TUI 语言包
- #4787 —— 本矩阵的 TUI 表以及区域设置漂移的 CI 门禁
- #4788 —— 法语、德语、加泰罗尼亚语的 TUI 本地化
- #4789 —— 印尼语本地化
- #4790 —— 印地语本地化 + 天城文终端排版验证
- #4791 —— 乌克兰语随俄语一起本地化
- #4749 —— 加泰罗尼亚语 UI 语言 + 加利西亚语/巴斯克语评估
- #5482 —— EPIC(docs)：审阅、部分重构并将文档完整本地化为中文
