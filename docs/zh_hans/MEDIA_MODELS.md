# 语音、翻译与视频模型

> 英文原文：[MEDIA_MODELS.md](../MEDIA_MODELS.md)。
> 源码与厂商官方文档核查：2026-10-06。下表区分现有能力与候选，不宣称候选已接入或经过真实服务验证。

## 当前源码已有能力

| 任务 | Codewhale 入口 | 限制 |
| --- | --- | --- |
| 文本转语音（TTS） | `speech` 工具；`codewhale --provider xiaomi-mimo speech "你好，Codewhale"`；CLI 别名 `tts` | 客户端要求 Xiaomi MiMo。支持 `mimo-v2.5-tts`、`mimo-v2.5-tts-voicedesign`、`mimo-v2.5-tts-voiceclone` 及旧 `mimo-v2-tts`。设计/克隆需对应输入。 |
| 语音转文字（STT/ASR） | 终端语音输入、Runtime `/v1/voice/*`；GPUI 复用该路径 | 显式 `CODEWHALE_ASR_MODEL` 优先，否则检测本地 Whisper → Groq → 配置的提供商。本地 `tiny`；Groq `whisper-large-v3-turbo`；提供商 `mimo-v2.5-asr`。聊天端点不一定支持 ASR。 |
| 视频理解 | 媒体附件/读取及适用提供商能力 | 读取视频、翻译字幕与生成视频是不同任务。 |
| 视频生成 | 本次核查未发现一等生成工具/客户端 | 远程目录列出模型不等于有可执行生成路由。 |

`CODEWHALE_ASR_MODEL=local-whisper` 显式选择离线识别，需先安装本地程序/模型。
`groq` 需 `GROQ_API_KEY`；`mimo` 使用当前配置的提供商端点。没有配置文件 ASR
选择器或任意模型 ID 覆盖。选定本地/Groq 后失败，不会静默上传给另一提供商；
把变量设为 Qwen/Index 模型名也不会安装后端。

源码 owner：[语音工具](../../crates/engine/src/tools/speech.rs)、
[合成客户端](../../crates/engine/src/client.rs)、[语音选择](../../crates/engine/src/voice.rs)、
[Runtime 语音](../../crates/tui/src/runtime_api/voice.rs)。真实麦克风、合成请求及完整
用户语音旅程仍需独立验收。

## Index-Translate 核查

[哔哩哔哩官方仓库](https://github.com/bilibili/Index-Translate)发布基于 Qwen3.5 的
150 语言文本翻译（Apache-2.0）、Homura 音节约束及 NativeLong 文档翻译。
NativeLong 实际 ID 是 `IndexTeam/Index-Nailong-2B` / `IndexTeam/Index-Nailong-9B`。
[官方集合](https://huggingface.co/collections/IndexTeam/index-translate)含量化版本；
Echo GGUF 只有文本骨干，不是完整音频管线。

宣告的文本 API 为 `https://index-translate.bilibili.com/v1`，模型
`Index-Translate-35B-A3B`。2026-10-06 小规模公开文本测试返回 **HTTP 412**，
明确表示访问策略拒绝；未发送凭据/私人内容。尚不能标作可用服务。

| 独立包 | 发布范围 | 接入状态 |
| --- | --- | --- |
| [Echo S2TT](https://github.com/bilibili/Index-Translate/blob/main/inference/echo-s2tt/README.md) | 中文音频/视频 → 英/日/西语时间轴字幕；2B 指南估计约 10 GB 显存 | 本地候选；不是公共文本 API 或通用 150 语言 STT。 |
| [Echo S2ST](https://github.com/bilibili/Index-Translate/blob/main/inference/echo-s2st/README.md) | 中文 → 英/西/日；英文 → 中/西/日，保留来源声音。CUDA GPU ≥12 GB（2B）/≥24 GB（9B）显存 | 配音任务候选；长视频编排另有管线，不生成新视频。 |

## 下一批候选

此顺序依据已有执行路径提出；尚未做付费测试。

| 优先级/任务 | 官方来源 | 适合的接入与缺少的证据 |
| --- | --- | --- |
| 1：本地多语 STT | [Qwen3-ASR](https://github.com/QwenLM/Qwen3-ASR)，0.6B/1.7B、Apache-2.0 | 在已有语音选择器旁评估中文/方言及本地适配器；改默认值前验证硬件、延迟与失败时隐私。 |
| 2：本地 TTS | [Qwen3-TTS](https://github.com/QwenLM/Qwen3-TTS)，0.6B/1.7B、Apache-2.0；[CosyVoice](https://github.com/QwenAudio/CosyVoice)，`Fun-CosyVoice3-0.5B-2512` | 经现有 Engine 权限评估合成、设计及获准参考声音，独立于聊天提供商。 |
| 2：时长受控配音 | [IndexTTS](https://github.com/index-tts/index-tts) | 官方使用 bilibili Model Use License Agreement；不能从 Index-Translate 或 fork 推断 Apache-2.0，需独立核对权重/条款。 |
| 3：视频生成 | [xAI 视频 API](https://docs.x.ai/developers/model-capabilities/video/generation)，`grok-imagine-video-1.5` | 异步 `/v1/videos/generations` 与轮询；需类型化费用/任务授权、取消、持久身份及输出工件验证，不能借用聊天请求。 |

复用现有 Engine、权限门禁与工件回执。固定已审查运行时/权重，执行前明确后端与
上传目的地，保留显式离线选择。下载、托管请求及声音样本上传仍需普通授权。
记录实际失败、语言方向、许可范围与硬件证据。参见[提供商](PROVIDERS.md)、
[工具](TOOL_SURFACE.md)、[插件](PLUGIN_AUTHORING.md)。
