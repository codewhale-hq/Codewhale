# Codewhale voice and terminal charter

> 阅读简体中文版：[zh_hans/VOICE.md](zh_hans/VOICE.md)。

This page is the writing charter for Codewhale's own messages. To dictate
with your microphone, see [Voice input](VOICE_INPUT.md).

Codewhale speaks like an instrument with a constitution: calm, exact, and
receipt-driven. It is maritime without nautical jokes. It names the action,
the boundary, and the next useful move.

## Voice

- Lead with the fact: `MCP tool pool reloaded in process`.
- Name the boundary: `Provider switching stays in /provider`.
- Offer one next action when recovery is possible.
- Prefer short sentences and concrete nouns over slogans or celebration.
- Say `saved`, `reloaded`, `verified`, or `failed` only when that event
  happened. An affordance is not a receipt.
- Keep product terms exact: Codewhale; Plan / Work; Ask / Accept Edits /
  Auto-Review / Full Access; Fleet / Workflow / Lane / Runtime.
- Keep commands, key names, paths, and provider/model names literal. Compose
  them in code around localized prose.

Avoid timer carousels, marketing banners, anthropomorphic chatter, emoji
celebrations, and borrowed competitor language. Guidance is action-triggered,
seen-gated, and quiet.

## Blue Stage

Blue Stage is the default visual grammar, not a separate product mode.

- Stage black holds the room.
- Action blue owns general interaction.
- Structural ice owns Plan.
- Seafoam and working green report live or successful state.
- Signal Gold is reserved for the whale and moments requiring human attention.

Theme settings remain compatible as `dark` and `light`; picker labels expose
the product names `Blue Stage` and `Blue Stage Light`.

## Glyphs

`crates/tui-kit/src/glyphs.rs` owns authored terminal glyphs and their narrow
ASCII fallbacks. Renderers consume semantic names rather than choosing symbols
locally.

- `●` is the recurring Codewhale anchor: current speaker or current human
  choice.
- `▸` is selection or active traversal.
- `◆` means attention or waiting, never generic decoration.
- `✓` and `✕` are settled success and failure.
- `▎` marks finished user input; `▏` is a transcript continuation rail.

The terminal compatibility layer applies ASCII fallbacks. Translation strings
do not own glyphs, commands, or key names.

## Copy review

For new customer-visible text:

1. Identify the typed fact that owns it.
2. Write the shortest truthful sentence in this register.
3. Add a `MessageId` and every complete locale when the TUI renders it.
4. Keep keys, commands, paths, and glyphs as code-owned placeholders.
5. Test narrow widths and the ASCII-safe renderer when chrome changes.
