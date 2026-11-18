# Codewhale 的语气与终端章程

> 英文原文：[VOICE.md](../VOICE.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

Codewhale 说话像一件自带宪章的仪器：平静、准确，一切以回执为准。
它有航海气质，但不讲航海笑话。
它会点明做了什么、边界在哪，以及下一步该做什么。

## 语气

- 先说事实：`MCP tool pool reloaded in process`。
- 点明边界：`Provider switching stays in /provider`。
- 还能恢复时，给出一个下一步动作。
- 多用短句和具体的名词，少用口号和庆功式的话。
- `saved`、`reloaded`、`verified`、`failed` 这几个词，只有事情真的发生了才能说。
  界面上有可点的入口，不等于事情已经发生。
- 产品术语必须一字不差：Codewhale；Plan / Work；Ask / Accept Edits /
  Auto-Review / Full Access；Fleet / Workflow / Lane / Runtime。
- 命令、按键名、路径，以及提供商（provider）和模型名都照原样写。
  由代码把它们和本地化后的文字拼在一起。

不要定时轮播，不要营销横幅，不要拟人化的闲聊，不要 emoji 庆祝，
也不要抄竞争对手的说法。引导由动作触发，用户看过一次就不再重复，并且保持安静。

## Blue Stage

Blue Stage 是默认的视觉语法，不是一种独立的产品模式。

- Stage black 撑起整个界面的底色。
- Action blue 负责一般交互。
- Structural ice 负责 Plan。
- Seafoam 与 working green 表示进行中或成功状态。
- Signal Gold 留给鲸鱼，以及需要人注意的时刻。

主题设置仍兼容 `dark` 和 `light`；选择器标签上显示的产品名是
`Blue Stage` 和 `Blue Stage Light`。

## 字形

终端自有的字形和窄版 ASCII 回退都由 `crates/tui-kit/src/glyphs.rs` 定义。
渲染器按语义名称取用，不在本地自己挑符号。

- `●` 是 Codewhale 反复使用的锚点：表示当前发言者，或当前由人做出的选择。
- `▸` 表示选中，或当前正在进行的遍历。
- `◆` 表示需要注意或正在等待，绝不拿来当装饰。
- `✓` 和 `✕` 表示已经有结果的成功与失败。
- `▎` 标记已经结束的用户输入；`▏` 是转录（transcript）里表示接续的竖轨。

终端兼容层会套用 ASCII 回退。字形、命令和按键名不由翻译字符串定义。

## 文案评审

用户能看到的新文案，按下面几步处理：

1. 先确认这条文案由哪条类型化的事实（typed fact）定义。
2. 用这种语域，写出最短而真实的一句话。
3. TUI 渲染它时，要补上 `MessageId` 和每一个完整的 locale（区域设置）。
4. 按键、命令、路径和字形都保留为占位符，由代码来填。
5. 界面外壳（chrome）有改动时，测试窄宽度和 ASCII 安全渲染器。
