# Speech, translation and video models

> 简体中文：[zh_hans/MEDIA_MODELS.md](zh_hans/MEDIA_MODELS.md)。
> Source and upstream documentation reviewed: 2026-10-06. Candidates below are
> not additional working Codewhale backends.

## Current source support

| Task | Codewhale path | Limits |
| --- | --- | --- |
| Text to speech (TTS) | `speech` tool; `codewhale --provider xiaomi-mimo speech "Hello from Codewhale"`; CLI alias `tts` | The synthesis client requires Xiaomi MiMo. Models: `mimo-v2.5-tts`, `mimo-v2.5-tts-voicedesign`, `mimo-v2.5-tts-voiceclone`, legacy `mimo-v2-tts`. Design/cloning need their corresponding input. |
| Speech to text (STT/ASR) | Terminal voice input, Runtime `/v1/voice/*`; GPUI uses that Runtime path | Explicit `CODEWHALE_ASR_MODEL`, otherwise detected local Whisper → Groq → configured provider. Local `tiny`; Groq `whisper-large-v3-turbo`; provider `mimo-v2.5-asr`. A chat endpoint does not prove ASR support. |
| Video understanding | Media attachment/read paths and applicable provider capabilities | Reading video, translating subtitles and generating video are separate operations. |
| Video generation | No first-class generation client/tool in the audited source | A remote catalogue row does not create an executable generation route. |

`CODEWHALE_ASR_MODEL=local-whisper` explicitly selects offline recognition;
its binary/model must already be installed. `groq` needs `GROQ_API_KEY`;
`mimo` selects the configured provider endpoint. There is no config-file ASR
selector or arbitrary model-ID override. A failed selected local/Groq backend
does not silently upload to another provider. Setting a Qwen/Index model name
in this variable does not install a new backend.

Owners: [speech tool](../crates/engine/src/tools/speech.rs),
[synthesis client](../crates/engine/src/client.rs), [voice resolver](../crates/engine/src/voice.rs),
[Runtime voice](../crates/tui/src/runtime_api/voice.rs). Real microphone capture,
synthesis and a complete customer voice journey require separate acceptance.

## Index-Translate evaluation

[Bilibili's repository](https://github.com/bilibili/Index-Translate) publishes
Qwen3.5-based text translation for 150 languages under Apache-2.0, Homura
syllable constraints and NativeLong document translation. NativeLong's actual
IDs are `IndexTeam/Index-Nailong-2B` / `IndexTeam/Index-Nailong-9B`.
The [official collection](https://huggingface.co/collections/IndexTeam/index-translate)
includes quantized releases; Echo GGUF is the text backbone only.

The advertised text API is `https://index-translate.bilibili.com/v1`, model
`Index-Translate-35B-A3B`. A bounded public-text probe on 2026-10-06 returned
**HTTP 412**, an explicit access-policy rejection. No credentials/private
content were sent. This endpoint is not qualified as a working service.

| Separate package | Published scope | Integration status |
| --- | --- | --- |
| [Echo S2TT](https://github.com/bilibili/Index-Translate/blob/main/inference/echo-s2tt/README.md) | Chinese audio/video → English/Japanese/Spanish timestamped subtitles; 2B guide estimates about 10 GB VRAM | Local pipeline candidate, not the public text API or generic 150-language STT. |
| [Echo S2ST](https://github.com/bilibili/Index-Translate/blob/main/inference/echo-s2st/README.md) | Chinese → English/Spanish/Japanese; English → Chinese/Spanish/Japanese, preserving source voice. CUDA GPU ≥12 GB (2B) / ≥24 GB (9B) VRAM. | Reviewed dubbing-job candidate; long-video orchestration is separate and does not generate a new video. |

## Other candidates

This proposed sequence follows existing execution paths; no paid test has run.

| Priority / task | Official source | Fit and remaining work |
| --- | --- | --- |
| 1: Local multilingual STT | [Qwen3-ASR](https://github.com/QwenLM/Qwen3-ASR), 0.6B / 1.7B, Apache-2.0 | Evaluate Chinese/dialect accuracy through a reviewed local adapter beside the voice resolver. Prove hardware, latency and failed-backend privacy before changing defaults. |
| 2: Local TTS | [Qwen3-TTS](https://github.com/QwenLM/Qwen3-TTS), 0.6B / 1.7B, Apache-2.0; [CosyVoice](https://github.com/QwenAudio/CosyVoice), `Fun-CosyVoice3-0.5B-2512` | Evaluate synthesis/design/permitted voice references through existing Engine permissions, independently of the chat provider. |
| 2: Duration-controlled dubbing | [IndexTTS](https://github.com/index-tts/index-tts) | Its official repository uses the bilibili Model Use License Agreement. Do not infer Apache-2.0 from Index-Translate or a fork; review selected weights/terms separately. |
| 3: Video generation | [xAI video API](https://docs.x.ai/developers/model-capabilities/video/generation), `grok-imagine-video-1.5` | Separate asynchronous `/v1/videos/generations` and result polling. Needs typed cost/task authority, cancellation, durable job identity and verified output artifacts. A chat adapter is insufficient. |

Reuse the Engine, permission gate and artifact receipts. Pin reviewed runtime/
weights, expose the backend/upload destination before execution and preserve an
explicit offline choice. Downloads, hosted calls and voice-reference uploads
require ordinary authorization. Record actual failures, language directions,
licence scope and hardware proof. Related contracts: [providers](PROVIDERS.md),
[tools](TOOL_SURFACE.md), [plugins](PLUGIN_AUTHORING.md).
