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
2. **Web API (search, playlists, other platforms).** Requires an authorized Spotify
   connection with suitable scopes; report missing access without asking the
   user to paste a token into chat:
   `GET api.spotify.com/v1/search`, `/v1/me/playlists`, `/v1/playlists/{id}`.

## Workflow
1. Prefer local control for transport (play/pause/next/previous/current track).
   Use the requested `play` or `pause` verb; `playpause` is a toggle and cannot
   safely be retried after an uncertain result. Verify player state afterward.
2. Use available Web API endpoints for search and playlist contents. Check
   current app-access restrictions; do not promise the restricted Recommendations
   API. Handle 403/429 responses without bypassing access or rate limits.
3. Report artist, track, album, and playlist names verbatim.

## Non-goals
- Do not change playlists (add/remove/reorder) unless asked.
- Do not download or rip audio.
- Do not handle the user's Spotify password.

Reference: [Spotify API changes](https://developer.spotify.com/blog/2024-11-27-changes-to-the-web-api).
