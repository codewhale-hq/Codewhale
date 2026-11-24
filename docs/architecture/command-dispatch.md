# Command Dispatch Architecture

**Target branch:** `main`
**Related EPIC:** [#2870](https://github.com/codewhale-hq/CodeWhale/issues/2870)
**Related issue:** [#2791](https://github.com/codewhale-hq/CodeWhale/issues/2791)
**EPIC-002 (Command Single Responsibility Extraction):** Layer 4.x (FEAT-006 through FEAT-008)

This document records the command-dispatch ownership model after the
command-boundary replay landed on `main`, updated through EPIC-002 (command
single responsibility extraction). It reflects the final layered ownership:
top-level group registration, group-owned command registration, and
command-level ownership of metadata and behavior. It is the public reference for the
module boundaries, dispatch precedence, and permanent exceptions that remain
after the command-boundary refactor.

## Dispatch Flow

`commands::execute()` owns the slash-command dispatch gate. The order is
intentional:

| Step | Source | Behavior |
|------|--------|----------|
| 0 | `$skill` compatibility | `$name` is resolved as `/skill name` before slash parsing. |
| 1 | User commands | `user_registry::try_dispatch()` checks workspace and global markdown commands first, so user commands can shadow built-ins. Workspace commands (`.codewhale`, `.deepseek`, `.claude`, `.cursor`) load only in a trusted workspace and never under the name or alias of a protected built-in — the commands that grant or revoke authority, hold credentials, or undo work, such as `/trust`, `/undo`, `/permissions`, `/mode`, `/config` (`PROTECTED_BUILTINS` in `user_registry.rs`); such a definition is skipped with a load error. |
| 2 | Permanent mode compatibility alias | `/jihua` routes through config mode dispatch so it selects Plan mode directly. It remains a registered alias for discovery, bypassing normal `/mode` execution. |
| 3 | Built-in registry | `CommandRegistry` resolves group-owned built-in commands by canonical name or alias. |
| 4 | Legacy migration hints | Retired commands such as `/set` and `/deepseek` return targeted replacement guidance. |
| 5 | Skills fallback | If no command matches, a skill with the same name may run before unknown-command suggestions are shown. |

## Module Boundaries

| Module | Responsibility |
|--------|----------------|
| `crates/tui/src/commands/mod.rs` | Central dispatch gate, registry initialization, public command lookup helpers, and unknown-command suggestions. |
| `crates/tui/src/commands/traits.rs` | Built-in command metadata, trait-backed command objects, command groups, and registry lookup. |
| `crates/tui/src/commands/groups/` | Group-owned built-in command areas. Each group owns its command metadata and handlers. |
| `crates/tui/src/commands/user_registry.rs` | User-command registry boundary: markdown metadata, aliases, hidden entries, validation errors, dispatch state resets, and shadowing behavior. |
| `crates/tui/src/commands/discovery.rs` | Shared discovery-shadowing contract consumed by both the command palette and slash completion: canonical-token shadowing, alias-token shadowing, and unshadowed-alias projection. Both discovery surfaces interpret user-command ownership identically through this module. |
| `crates/tui/src/commands/user_commands.rs` | Lower-level file scanning, frontmatter parsing, allowed-tools parsing, and template substitution used by the registry. |
| `crates/tui/src/tui/command_palette.rs` | Palette entries for built-ins and visible user commands, with user commands shadowing built-ins. |
| `crates/tui/src/tui/widgets/mod.rs` | Slash completion, user-command metadata display, and alias-shadowing behavior. |

## Built-In Command Groups

| Group | Scope |
|-------|-------|
| `core` | Help, model/provider selection, queue, hooks, subagents, links, feedback, voice, and core navigation. |
| `config` | Config, settings, status surfaces, mode, theme, trust, logout, and related settings commands. |
| `debug` | Token/cost introspection, cache, system/context, diff/edit, undo, and retry. |
| `memory` | Persistent memory and notes. |
| `plugins` | Read-only bundle discovery/validation plus explicit trust, enable, disable, revoke, and reload lifecycle commands; legacy executable tools remain separate. |
| `project` | Project initialization, sharing, LSP, and goal/hunt commands. |
| `session` | Rename, save, fork/new/load sessions, compaction, purge, relay, and export. |
| `skills` | Skills Manager (`/skills`), text inspect/remote/sync paths, activation (`/skill`), and managed install/update/uninstall/trust. |
| `utility` | Attachments, tasks/jobs, MCP, and network. |

## User Commands

User commands are markdown files loaded from these locations in precedence
order:

1. `<workspace>/.codewhale/commands/`
2. `<workspace>/.deepseek/commands/`
3. `<workspace>/.claude/commands/`
4. `<workspace>/.cursor/commands/`
5. `~/.codewhale/commands/`
6. `~/.deepseek/commands/`

Supported frontmatter fields:

| Field | Meaning |
|-------|---------|
| `name` | Canonical slash-command name. It is normalized without a leading slash and replaces the filename-derived default. |
| `description` | Work objective and UI description. |
| `usage` | Preferred user-facing invocation syntax shown in the palette and slash completion. |
| `arguments` | Argument synopsis and a signal that selection should leave the composer open for input. It does not impose runtime validation. |
| `argument-hint` | Backward-compatible palette/completion hint for expected arguments. It remains the display fallback when `usage` is absent. |
| `allowed-tools` | Restricts command execution tools. An explicit empty value blocks all tools. |
| `pausable` | Marks the command as pause/resume capable. |
| `alias` / `aliases` | Additional user-command names. A workspace command's alias cannot claim a protected built-in. |
| `hidden` | Hides the command from palette/completion while allowing direct dispatch. |

The canonical name defaults to the normalized markdown filename. A valid
frontmatter `name` replaces that default; the filename is not retained as an
implicit alias, so a renamed command must list the old filename under `alias`
or `aliases` if both spellings should dispatch. A configured name may include
one leading slash for readability, but after normalization it must be one
non-empty slash-command token with no whitespace or embedded `/`. An invalid
configured name is a recoverable error attached to the filename-derived
command, preventing silent fallthrough to a built-in.

Source precedence is resolved before frontmatter naming: a higher-precedence
directory wins when the same filename exists in more than one location. Files
inside each directory are ordered by normalized filename. If distinct files
then resolve to the same frontmatter `name`, the first file in that stable
directory-and-filename order wins and the losing file records a recoverable
load error. Aliases cannot replace any canonical user-command name; duplicate
aliases also use first-wins order. Errors on a losing duplicate never poison a
valid winning definition.

Presentation metadata has an explicit fallback order: non-empty `usage`, then
non-empty legacy `argument-hint`, then non-empty `arguments`. `arguments` and
`argument-hint` also cause palette/menu selection to append a space; `usage`
does so when it describes more than the bare command name. These fields do not
parse, require, or reject invocation arguments. Runtime expansion remains
backwards-compatible: `$ARGUMENTS` receives the complete argument tail and
`$1`, `$2`, and so on receive whitespace-separated positional values.

Malformed files remain registered under their resolved name with a
dispatch-time error, so they cannot silently fall through to a built-in. Their
errors are isolated per file: valid siblings still load, appear, and dispatch.
Hidden commands participate in shadowing and remain directly dispatchable, but
are removed from palette and slash-completion discovery.

Explicit `/help <name>` topics and unknown-command typo suggestions resolve
through the same user-command precedence as execution. When a user command
owns a built-in name or alias, help shows the user command's metadata and typo
suggestions point to its canonical name rather than the shadowed built-in.

Dispatch through `user_registry` resets stale command state before sending the
new command body: hunt objective fields, token/time counters, continuation
count, allowed tools, pause state, todos, and plan state.

## Permanent Exceptions

| Exception | Rationale |
|-----------|-----------|
| `/jihua` | Backward-compatible mode alias that predates the group-owned registry. It routes through config mode dispatch to preserve its fixed Plan mode selection. |
| `/set` and `/deepseek` migration hints | Retired commands kept only as direct typed guidance. They are excluded from registry and autocomplete. |
| `#[allow(clippy::module_inception)]` in matching group modules | Group directories intentionally contain same-named child modules such as `core/core.rs`. |
| `user_commands.rs` lower layer | The registry owns runtime behavior, while this module remains the shared filesystem and parser layer. |
| `#[cfg(test)]` helpers in `user_commands.rs` | Deferred test migration compatibility while registry-specific tests are added. |

## EPIC-002 Completion Status (closed)

EPIC-002 (Command Single Responsibility Extraction) extracted commands for
all 9 command groups through Layer 4.x sublayers. Layer 4.2 (FEAT-008) landed as
PR #3706 (merged) and issue #2870 closed on 2026-08-01.

| Layer | FEAT | Title | Status |
|---|---|---|---|
| 4 | FEAT-006 | Core, Config, Session, and Debug Command Extraction | Complete |
| 4.1 | FEAT-007 | Project, Memory, Skills, Utility, and Plugins Extraction | Complete |
| 4.2 | FEAT-008 | Registry Cleanup, Documentation, and Full Validation | Complete |

## Replay Status (EPIC-001)

FEAT-001's group-owned built-in command direction is represented on `main` by
the newer trait-backed registry and nested group tree. FEAT-002 is replayed as
the dedicated user-command registry boundary. FEAT-003 is replayed as public
architecture and PR/issue evidence documentation, updated for the current
`main` target instead of the old `release/v0.8.60` branch.
