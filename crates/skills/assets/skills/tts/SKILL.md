---
name: tts
description: Speak or render text to speech locally. Use when: read aloud, speak, voice output, tts, text to speech, or an audio version of text.
invocation: model+user
---

# Text to Speech

## When to use
Reading text aloud or rendering speech audio files, using only the voices
already on the machine.

## Setup
No account or API key. Pick the platform command; fail loud when the
Linux engine is missing (`sudo apt install speech-dispatcher` or `espeak-ng`):

- macOS: `say -v Samantha -o out.aiff "text"` (list voices: `say -v '?'`)
- Linux: `spd-say "text"` or `espeak-ng -w out.wav "text"`
- Windows: PowerShell `System.Speech.Synthesis.SpeechSynthesizer`

## Workflow
1. Keep utterances short; split long text at paragraph breaks.
2. Confirm voice and speed when the user cares; otherwise default voice, normal rate.
3. For files, report the path, format, and duration.

## Non-goals
- Do not clone voices or impersonate a real person.
- Do not call cloud TTS APIs (different cost and privacy posture — ask first).
- Do not play audio unprompted in shared environments; render a file instead.
