# User Memory

> 阅读简体中文版：[zh_hans/MEMORY.md](zh_hans/MEMORY.md)。

User memory gives the model a small, persistent, local store of
preferences and conventions that should survive across sessions —
"I prefer pytest over unittest", "this codebase uses 4-space
indentation" — without repeating them in every conversation.

As of v0.9.4 the **native memory store** is the only memory system.
It is Markdown files indexed by SQLite FTS5, fully offline, scoped by
a hash of the repo's git origin. The legacy single-file
(`~/.deepseek/memory.md`) push/inject path and the planned Moraine MCP
backend were both removed: no Moraine server ever shipped in-repo,
and the native store already provides the same architecture (durable
Markdown source of truth plus a rebuildable search index).

Memory is **opt-in**. When disabled (the default), nothing is loaded,
nothing is intercepted, and the `remember` tool isn't surfaced to the
model.

## Enabling memory

Either set the env var:

```bash
export DEEPSEEK_MEMORY=on
```

Accepted truthy values are `1`, `on`, `true`, `yes`, `y`, and
`enabled`.

…or add to `~/.codewhale/config.toml`:

```toml
[memory]
enabled = true
```

Restart the TUI after toggling. Disabling is the same in reverse.

## Layout

The store lives under a `memory/` directory next to where the legacy
`memory_path` anchor points — by default `memory_path = "~/.codewhale/memory.md"`
re-roots to `~/.codewhale/memory/`:

```text
~/.codewhale/memory/
├── global/MEMORY.md        # user-scoped notes (follow you everywhere)
├── workspace/<id>/MEMORY.md   # repo-scoped notes (hash of git origin)
└── index.sqlite3           # rebuildable SQLite FTS5 cache
```

The scope directory is `workspace` (singular) — `MemoryScope::directory`,
`crates/runtime/src/native_memory.rs:31-36`. The index filename is
`index.sqlite3` (`native_memory.rs:175`).

Markdown is the durable source of truth; `index.sqlite3` is a disposable
full-text cache (`/memory native reindex` rebuilds it). A configured
`memory_path` is an **anchor only**: the filename is discarded and its
parent gains the `memory/global/MEMORY.md` tree. Do not set
`memory_path` to the native layout path itself — that double-nests the
tree. The shipped example keeps `~/.codewhale/memory.md` so the store
lands at `~/.codewhale/memory/global/MEMORY.md`.

## What gets injected

When memory is enabled, the system prompt carries a bounded,
provenance-bearing block of memory entries (up to 32 entries /
12,000 chars, global plus current-workspace scope). The block is
wrapped to mark it as **untrusted user data**, not a second
instruction layer. For depth beyond the injected head, the model can
call the `memory_search` / `memory_get` tools against the FTS5 index.

The initial memory snapshot belongs to the session's frozen prompt prefix.
Changes discovered during the session are appended as history, as described
in [CACHE.md](CACHE.md); neither a native note update nor an external recall
may rewrite the system prompt or tool catalog on every turn.

Sub-agents inherit the same packet: a child's system prompt carries the
parent session's block plus the `## Memory Hygiene` guidance, so pinned rules
reach the agent that actually edits files. The child re-composes the packet
from the inherited `memory_path` (no separate flag, no empty envelope) and
records no context receipt of its own. See
[SUBAGENTS.md](SUBAGENTS.md#memory-and-the-remember-tool-489).

## Three ways to add to memory

### 1. The `# ` composer prefix (#492)

Type a single line that starts with `#` (but not `##` or `#!`) in
the composer:

```
# remember to use 4-space indentation in this repo
```

The TUI intercepts the input and appends the note to the **global**
native store via the same `NativeMemoryStore::remember` path the
model's tool uses. **No turn fires** — your input is consumed, the
status line confirms the file it wrote to, and you can keep typing
your real question.

Multi-`#` prefixes deliberately fall through to normal turn
submission so you can paste Markdown headings without surprise.

### 2. The `/memory` slash command

Inspect and maintain the native store:

`/memory` splits in two. The bare subcommands operate on the single file at
`config.memory_path()`; everything about the native store lives behind
`/memory native …` (`crates/tui/src/commands/groups/memory/memory.rs:236-268`).

| Subcommand      | Effect                                                    |
|-----------------|-----------------------------------------------------------|
| `/memory`       | Print the path and contents of the `memory_path` file      |
| `/memory show`  | Same as bare `/memory`                                     |
| `/memory path`  | Print the `memory_path` file location                      |
| `/memory clear` | Truncate that file                                         |
| `/memory edit`  | Print the `$EDITOR` invocation for it                      |
| `/memory help`  | Show command-specific help                                 |

Anything else returns `unknown subcommand`. The native store is reached through
the `native` prefix (`memory.rs:221`):

| Subcommand                              | Effect                              |
|-----------------------------------------|-------------------------------------|
| `/memory native status`                 | Store root, active source, index    |
| `/memory native path`                   | Native store root                   |
| `/memory native remember [global\|workspace] <note>` | Append a note          |
| `/memory native search <query>`         | FTS5 search                         |
| `/memory native get <id>`               | Read one entry                      |
| `/memory native reindex`                | Rebuild the FTS5 index              |
| `/memory native import`                 | Import the legacy single-file store |
| `/memory native export`                 | Dump entries                        |
| `/memory native delete [all\|global\|workspace]` | Delete entries             |

There is no `/memory add` and no bare `/memory reindex`; use
`/memory native remember` and `/memory native reindex`.

### 3. The `remember` tool (auto-capture, #489)

When memory is enabled the model gets a `remember` tool:

```json
{
  "name": "remember",
  "input_schema": {
    "type": "object",
    "properties": {
      "note":  { "type": "string" },
      "scope": { "type": "string", "enum": ["global", "workspace"] }
    },
    "required": ["note"]
  }
}
```

The model uses this when it notices a durable preference, convention,
or fact worth keeping across sessions. The tool is auto-approved
because writes are scoped to the user's own memory files — gating
them behind the standard write-approval flow would defeat the point
of automatic memory capture. Workspace scope requires a git
repository with an `origin` remote (the scope id is a hash of it).

## What stays out of memory

Memory is for **durable** signal. Things that should NOT live there:

- **Secrets** — no API keys, tokens, passwords. The files are plain
  text on disk and entries are injected into the system prompt.
- **Transient task state** — "I'm currently working on the parser"
  changes every session; it doesn't belong in cross-session memory.
- **Conversation snippets** — quote-style notes belong in the notes
  tool (`note`), not memory.
- **Long instructions** — anything over a few sentences should live
  in `AGENTS.md` (project-level) or in a skill.

## Privacy and scope

The native store lives on your machine and does not synchronize itself to
a cloud memory service. Recalled entries are sent to the selected model
provider as prompt context when memory is enabled. Keep secrets out of it.
Workspace-scoped memory is keyed by a hash of the repo's git
origin, so notes from one repo never leak into another repo's prompt.

## External memory services

Persistent memory already works through the native store. First-class backend
selection currently accepts only `native` and `off`; there is no supported
`external`, `mem0`, or `memcode` backend setting.

A third-party memory service can expose tools through the existing
[MCP](MCP.md) or [plugin](PLUGINS.md) integration. Those are the service's own
tools: they do not replace `remember`, `memory_search`, `/memory native`, or
the `#` quick-add path. Review the plugin's permissions and the service's data
destination before enabling it. An unavailable external service must report
its failure rather than silently send notes to another backend.

A future first-class backend must cover capture, search, correction, deletion,
scope and error reporting across every memory entry point. It must also keep
volatile recall in append-only history under the cache contract above. That
complete migration is tracked in [#6050](https://github.com/codewhale-hq/CodeWhale/issues/6050);
0.9.13 does not claim that migration or a commercial memory integration.

## Configuration reference

```toml
# ~/.codewhale/config.toml
[memory]
enabled = true                    # default false; or set DEEPSEEK_MEMORY=on
# Optional explicit backend selection:
# backend = "native"              # "native" or "off" (default: off)
```

| Setting               | Default                       | Override                              |
|-----------------------|-------------------------------|---------------------------------------|
| Memory enabled        | `false`                       | `[memory] enabled = true` or `DEEPSEEK_MEMORY=on` |
| Backend               | `off`                         | `[memory] backend = "native"`         |
| Store root            | `~/.codewhale/memory/`        | derived from `memory_path`            |

## Related

- `docs/SUBAGENTS.md` — sub-agents inherit memory and can use the
  `remember` tool too.
- `docs/CONFIGURATION.md` — full config reference.
- Issue [#489](https://github.com/codewhale-hq/CodeWhale/issues/489)
  — phase-1 EPIC tracking the work.
