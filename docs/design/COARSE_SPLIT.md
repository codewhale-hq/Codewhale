# Coarse split of codewhale-tui — 2026-10-31

Status: stage A pre-move work landed; the crate moves have not started.
Founder, 2026-10-31: make the top line of crates ~400–500k lines, like Codex
and Grok Build, instead of going straight to ~10k crates. Measured with
`scripts/split/crate_map.py`, using the same units as
[CRATE_MAP.md](CRATE_MAP.md).

## Why coarse first

| Repo | Rust packages | Largest crates (lines incl. tests) |
| --- | ---: | --- |
| Codex `codex-rs` (2026-10-19) | 162 | `core` 463k, `tui` 463k, `app-server` 197k |
| Grok Build (2026-09-29) | 104 | `xai-grok-pager` 551k, `xai-grok-shell` 430k, `xai-grok-tools` 165k |
| Codewhale today | 51 | `tui` 1.18M (570k production) |

The 66-crate map needs 411 upward refs fixed before extraction, and a
read-only review (Codex CLI, `gpt-6.1-sol`) found more it cannot see: inherent
`impl Event`/`impl Op` split from their types (`core/protocol_parity.rs:1306,
1314`), reciprocal glob coupling between `tui/ui/overlays.rs` and
`tui/ui/handlers.rs`, a lexical `cached_command_list!` macro, `#[path]` mounts,
`pub(super)` fields, `build.rs` env vars and embedded assets. Almost all of
that lives *inside* a band. Cutting only at the band boundaries avoids it.

## Stage A: three crates

Map: [`crate-map-coarse.toml`](crate-map-coarse.toml).

| Crate | Content | Production lines | Lines incl. tests (est.) |
| --- | --- | ---: | ---: |
| `codewhale-engine` (new) | engine and the one turn loop, tool registry and tools, agents, context and `BASE_PROMPT`, the session store and the runtime thread manager, model client/catalog, MCP, extension host, fleet, tasks | 308,209 | ~620k |
| `codewhale-app-server` (existing, 16k today) | transports: runtime API and routes, ACP/MCP/exec hosts, chat relay and remote control | 43,195 | ~70k |
| `codewhale-tui` (what remains) | TUI, slash commands, doctor, and the `lib.rs` composition root | 219,133 | ~430k |

Dependencies run `engine ← app-server ← tui`; `codewhale-tui` already
depends on `codewhale-app-server`, never the reverse. The thread manager sits
in the engine as Codex's `ThreadManager` sits in `codex-core`; app-server keeps
only transports, which matches the App Server v2 plan.

Checker on the current tree: `cycles=0 upward_edges=0 upward_refs=0
oversize=0 unassigned=0 duplicates=0`. What got there:

1. **Checker fixes.** `PATH_RE` read `super::supervisor` as `super::super`
   plus `visor`, sending real references to the crate root; and grouped
   imports (`use crate::a::{b::C, d}`) were not followed. Both fixed. The
   grouped-import fix exposed that `task_manager` uses
   `RuntimeThreadManager`/`SharedRuntimeThreadManager` and that
   `remote_control` uses the chat relay's types.
2. **Band placement.** `runtime_threads/manager` and `tools/registry` belong
   with the engine (the engine, children and tasks construct and drive them);
   `remote_control` with app-server (only hosts and the TUI use it); `core/protocol_parity` with the engine, so its `impl Event` and `impl Op` stay in the crate that defines those types (the only split inherent impl a scan of `impl` blocks found);
   `doctor` and `doctor_fix` with the TUI (the doctor command lives in
   `lib.rs`).
3. **Code.** `runtime_account_api_base` and its validator moved from
   `runtime_api` to `profile_constitution`, which the engine calls; and
   `client.rs` names `codewhale_runtime::config` instead of the `lib.rs`
   re-export.

### The moves, still to do

Cross-crate items once the engine is its own crate (counted by the checker,
so a floor): the TUI names ~530 distinct engine items over ~1,150 refs, most
of them `pub(crate)` today. Each move therefore needs, in one slice:

- `git mv` of the engine band into `crates/engine/src`, keeping module paths,
  then `crate::` paths in the TUI rewritten to `codewhale_engine::`.
- `pub(crate)` widened to `pub` only for items the TUI or app-server name;
  `pub(super)` and private mutation authority stay as they are (review #10).
- The engine's own `test_support` (the env-scope and home fixtures it uses
  under `cfg(test)`), separate from the TUI's App fixtures.
- Cargo manifest, `build.rs` env vars (`CODEWHALE_BUILD_VERSION`,
  `option_env!` in agent receipts), feature forwarding, embedded assets
  (extension bundles, browser assets, changelog, pet-watch TSV), and the
  guard tests (`single_turn_loop` and others) moved with their code.
- Inherent `impl` blocks and orphan-rule trait impls checked across the new
  boundary before the move, since the checker cannot see them.

Then the host band moves into `crates/app-server/src` the same way. Each move
is proven with `cargo check --tests` on the affected crates and the existing
guard tests; at most two native builds at once.

## Stage B: split the engine near 500k

Stage A's engine is ~620k lines with tests. Stage B moves the lower half of
the runtime band (CRATE_MAP layers 1–17: MCP, model catalog and client,
billing, route, tasks, fleet, tool contract and runtime, tool families,
context, authority, agents, session store) into `codewhale-engine-tools`.

Map: [`crate-map-coarse-split.toml`](crate-map-coarse-split.toml).

| Crate | Production lines |
| --- | ---: |
| `codewhale-engine-tools` | 220,294 |
| `codewhale-tui` | 219,133 |
| `codewhale-engine` | 87,915 |
| `codewhale-app-server` | 43,195 |

Checker: `upward_edges=1 upward_refs=73`, all `engine-tools → engine`:
`HumanDecision`/`LivePosture`/`LiveRuntimeAuthority` (CRATE_MAP P1 and review
#7), the RLM caller (`RlmMode`, `CapturedRlmCaller`), `ExtensionHostManager`
and `StockOperation` reached by tools, hooks, MCP and fleet (CRATE_MAP P3), and
`task_manager` driving the thread manager. Those are the packets the fine map
needs anyway.

## After that

Further splits happen inside one coarse crate at a time, where churn or build
time says they pay: CRATE_MAP's runtime packets inside the engine crates, its
T packets inside the TUI. The review's findings are the entry criteria.

## Known limitations

- "Lines incl. tests" is estimated by assigning each source file to its unit;
  about 146k lines of `crates/tui/src` did not map to a unit and are left out.
- The checker resolves paths textually. Second-hop names, glob re-exports,
  macros and `#[path]` includes are still not followed, so zero upward refs
  is necessary, not sufficient; `cargo check` is the authority.
- Names (`codewhale-engine`, `codewhale-engine-tools`) are placeholders;
  `crates/core` and `crates/tools` already exist with other content.
