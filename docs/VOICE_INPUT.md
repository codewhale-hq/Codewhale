# Voice input

Dictate into the composer instead of typing. Codewhale records from your
microphone with a command-line recorder, sends the audio to one speech-to-text
backend, and puts the text at the cursor. Nothing is recorded until you run a
voice command.

This page covers the terminal. How Codewhale writes its own messages is in the
[voice and terminal charter](VOICE.md). Speech synthesis and the other media
routes are in [Media models](MEDIA_MODELS.md).

## Before you start

Install one recorder. Codewhale uses the first one it finds:

| System | Recorders tried, in order |
| --- | --- |
| macOS | `sox`, `rec` |
| Linux | `arecord`, `sox` |
| Windows | `sox` |

If none is installed, `/voice` answers `Voice: no recording tool found.
Install sox, arecord, or rec.` and records nothing.

## Commands

| Command | What it does |
| --- | --- |
| `/voice` | Records one dictation and inserts the text at the cursor. Aliases: `/yuyin`, `/语音`. |
| `/voicesend` | Turns auto-send on or off. Aliases: `/voice-send`, `/yuyinsend`, `/语音发送`. |
| `/voicecontrol` | Turns voice control on or off. Aliases: `/voice-control`, `/yuyincontrol`, `/语音控制`. |

The hotbar action `voice.toggle` runs `/voice`.

### Dictate

Run `/voice` and speak. The status line shows `Recording... speak now · pause to
finish`, and a preview of the words appears in the composer while you talk.
Recording stops after one second of silence or after 10 seconds, whichever
comes first. No key stops it early. The final text replaces the preview and
you can edit it before you send.

Each `/voice` is one recording. Run it again for the next sentence.

### Send by voice

With auto-send on, a dictation that ends in `send it`, `发送` or `發送` is
submitted without the phrase:

- "ship the fix, send it" submits `ship the fix`.
- "send it" alone submits what is already in the composer.

Without one of those phrases the text is inserted as usual and nothing is
sent. `/voicesend` again turns it off.

### Voice control

With voice control on, the speech model also receives your current draft as
context for the transcription, for example the names and terms you have
already typed. The result is still inserted at the cursor; your draft is not
rewritten. This works only with the configured-provider backend below, which
then uses the model `mimo-v2.5`. The local Whisper, Groq and DeepSeek backends
never see the draft and return a plain transcription.

## Where the audio goes

One backend handles each recording. Codewhale picks it in this order and does
not change your chat provider:

1. The backend named in `CODEWHALE_ASR_MODEL`, when set.
2. Local Whisper, when a `whisper`, `whisper.cpp`, `whisper-cpp` or
   `faster-whisper` binary is on `PATH`. The audio stays on your machine.
3. Groq, when `GROQ_API_KEY` is set (`whisper-large-v3-turbo`).
4. DeepSeek, when the active provider is `deepseek` or `deepseek-anthropic`
   (`deepseek-asr`).
5. The active provider's endpoint, with the model `mimo-v2.5-asr`.

`CODEWHALE_ASR_MODEL` accepts:

| Value | Backend |
| --- | --- |
| `local-whisper` | Local Whisper with the `tiny` model. The binary and model must already be installed. |
| `groq` | Groq. Needs `GROQ_API_KEY`. |
| `deepseek` | DeepSeek Tools speech recognition. Needs a DeepSeek key. |
| `mimo` | The active provider's endpoint with `mimo-v2.5-asr`. |

Any other value is ignored and the order above applies. The variable selects a
backend, not an arbitrary model id, and there is no config-file setting for it.

If the selected backend fails, Codewhale reports the failure. It does not
upload the audio to a different backend.

## Turn the microphone off

Set `CODEWHALE_DISABLE_VOICE=1` to disable microphone capture on this machine.
`/voice` then behaves as if no recorder were installed. Audio that a client
uploads through the Runtime API is not affected; see
[Runtime API](RUNTIME_API.md).

## Messages you may see

| Message | Meaning |
| --- | --- |
| `Voice: no recording tool found. Install sox, arecord, or rec.` | No recorder on `PATH`, or the microphone is disabled. |
| `Voice: no API key configured for the active provider` | The backend that would run needs a key you have not set. Set the key, or choose another backend with `CODEWHALE_ASR_MODEL`. |
| `Voice: no speech detected, recording too short` | The recorder heard nothing above the speech threshold. |
| `Voice: transcription request failed` | The backend returned an error; the reason follows the message. |
| `Voice: nothing to send` | The transcript was empty, or you said "send it" with an empty composer. |

## From another client

The Runtime API exposes the same recorder and backend selection under
`/v1/voice`, and accepts uploaded audio when the host has no microphone. The
routes are listed in [Runtime API](RUNTIME_API.md).
