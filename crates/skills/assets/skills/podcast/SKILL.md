---
name: podcast
description: Turn research into a spoken podcast — script, synthesize, stitch. Use when: podcast, generate a podcast, audio briefing, narrate, or turn this into audio.
invocation: model+user
---

# Podcast

## When to use
Producing a spoken-audio episode from a topic, document set, or briefing —
the full pipeline from research to an mp3.

## Setup
Requires `ffmpeg` for stitching (`brew install ffmpeg`, `apt install
ffmpeg`). Voices come from the `tts` skill. Fail loud when `ffmpeg` is missing.

## Workflow
1. Research the topic and draft a spoken script (~150 words per minute of
   audio). Read it back for approval before synthesizing.
2. Synthesize per segment with `tts`; keep segments under 2 minutes each.
3. Stitch: `ffmpeg -f concat -safe 0 -i segments.txt -codec:a libmp3lame out.mp3`.
4. Report duration, segment count, and the output path.

## Non-goals
- Do not publish or upload the episode anywhere.
- Do not synthesize more than the approved script.
- Do not clone real people's voices (see `tts` non-goals).
