---
name: spotify
description: Control Spotify playback and search the library and playlists. Use when: spotify, music, playlist, play, pause, or what's playing.
invocation: model+user
---

# Spotify

## When to use
Playing, pausing, skipping, identifying the current track, searching for
music, and reading playlists.

## Setup
Two paths:

1. **Local control (macOS, no setup).** The Spotify desktop app must be
   running, or say so:
   `osascript -e 'tell application "Spotify" to playpause'`
2. **Web API (search, playlists, other platforms).** Needs a Spotify
   developer app token from the user; fail loud when absent:
   `GET api.spotify.com/v1/search`, `/v1/me/playlists`, `/v1/playlists/{id}`.

## Workflow
1. Prefer local control for transport (play/pause/next/previous/current track).
2. Use the Web API for search, playlist contents, and recommendations.
3. Report artist, track, album, and playlist names verbatim.

## Non-goals
- Do not change playlists (add/remove/reorder) unless asked.
- Do not download or rip audio.
- Do not handle the user's Spotify password.
