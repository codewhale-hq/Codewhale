# Coarse split of codewhale-tui — 2026-10-31

Status: stage A part 1 is implemented and locally validated (CW-7701). Stage A part 2 moves the host band from `crates/tui` into the existing `crates/app-server` (CW-7764), with direct callers and no TUI module, summary or listener reexport facade. This source relocation keeps Engine as the sole turn, thread, store, event and prompt authority. Native qualification of the frozen move, Stage B and App Server v2 (CW-7704) remain separate gates; this is not a published release.
Founder, 2026-10-31: make the top line of crates ~400–500k lines, like Codex
and Grok Build, instead of going straight to ~10k crates. Measured with
`scripts/split/crate_map.py`, using the same units as
[CRATE_MAP.md](CRATE_MAP.md).

## Why coarse first

| Repo | Rust packages | Largest crates (lines incl. tests) |
| --- | ---: | --- |
| Codex `codex-rs` (2026-10-19) | 162 | `core` 463k, `tui` 463k, `app-server` 197k |
| Grok Build (2026-09-29) | 104 | `xai-grok-pager` 551k, `xai-grok-shell` 430k, `xai-grok-tools` 165k |
| Codewhale before stage A | 51 | `tui` 1.18M (570k production) |

The 66-crate map needs 411 upward refs fixed before extraction, and a
read-only review (Codex CLI, `gpt-6.1-sol`) found more it cannot see: inherent
`impl Event`/`impl Op` split from their types (`core/protocol_parity.rs:1306,
1314`), reciprocal glob coupling between `tui/ui/overlays.rs` and
`tui/ui/handlers.rs`, a lexical `cached_command_list!` macro, `#[path]` mounts,
`pub(super)` fields, `build.rs` env vars and embedded assets. Almost all of
that lives *inside* a band. Cutting only at the band boundaries avoids it.

## Stage A: three crates

Map: [`crate-map-coarse.toml`](crate-map-coarse.toml).

| Crate | Content | Planning production lines | Lines incl. tests (est.) |
| --- | --- | ---: | ---: |
| `codewhale-engine` (new) | engine and the one turn loop, tool registry and tools, agents, context and `BASE_PROMPT`, the session store and the runtime thread manager, model client/catalog, MCP, extension host, fleet, tasks | 308,209 | ~620k |
| `codewhale-app-server` (existing, 16k today) | transports: runtime API and routes, ACP/MCP/exec hosts, chat relay and remote control | 43,195 | ~70k |
| `codewhale-tui` (what remains) | TUI, slash commands, doctor, and the `lib.rs` composition root | 219,133 | ~430k |

The intended dependencies run `engine ← app-server ← tui`; `codewhale-tui` already
depends on `codewhale-app-server`, never the reverse. The thread manager sits
in the engine as Codex's `ThreadManager` sits in `codex-core`; app-server keeps
only transports, which matches the App Server v2 plan.

The checker reads Engine, App Server and TUI source roots and gives each Root a distinct ownership identity. Qualified `codewhale_app_server` paths resolve to their actual host owner. Custom boundary-check fixture roots stay hermetic. The previous pre-move measurement was 310,401 Engine, 41,829 host and 219,597 TUI production lines, 2,146 cross-band references and `cycles=0 upward_edges=0 upward_refs=0 oversize=0 unassigned=0 duplicates=0`; these historical mapped-band counts are not current workspace or compiler results. Re-measure both maps after the frozen move rather than copying these counts forward.

The lower Runtime owner and Paths socket layout remain the existing owners. App Server now depends on Engine and TuiKit (only the moved summaries use Kit), with no reverse Engine or Sandbox path to App Server. The move carries 48 current Rust sources, seven browser assets and one Node fixture, plus the complete Root exec helper/summary closure and existing tests. Current counts and hashes belong in the owner-run source receipt, not this planning table.

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

### Part 1: Engine extraction

The pre-move checker counted about 530 distinct Engine items over about
1,150 TUI references. Part 1 moves the modules and their existing fixtures,
updates consumers and path-sensitive guards, and separates shared test
helpers from the Engine's test modules. The `test-support` feature keeps
dependent tests isolated from real homes, keychains and provider probes.
The coarse crate exports its module APIs; private fields and `pub(super)`
approval constructors remain private. Internal-only exports can narrow as
stage B establishes smaller crate boundaries.

The extraction checklist is:

- `git mv` of the engine band into `crates/engine/src`, keeping module paths,
  then `crate::` paths in the TUI rewritten to `codewhale_engine::`.
- Cross-crate APIs exported for the TUI and app-server;
  `pub(super)` and private mutation authority stay as they are (review #10).
- The engine's own `test_support` (the env-scope and home fixtures it uses
  under `cfg(test)`), separate from the TUI's App fixtures.
- Cargo manifest, `build.rs` env vars (`CODEWHALE_BUILD_VERSION`,
  `option_env!` in agent receipts), feature forwarding, embedded assets
  (extension bundles, browser assets, changelog, pet-watch TSV), and the
  guard tests (`single_turn_loop` and others) moved with their code.
- Inherent `impl` blocks and orphan-rule trait impls checked across the new
  boundary before the move, since the checker cannot see them.

### Part 2: host extraction

The host modules and their children live in `crates/app-server/src`: ACP, MCP, exec, eval, runtime API, remote setup/control and runtime chat relay. CLI and TUI callers import their actual canonical owners. Listener handoff retains the same existing singleton in `runtime_api::listener`; summaries have one implementation in `exec_agent`, while preserved output, Pager and source copying stay in TUI. Existing route/history/debug entrypoints expose only the data and methods their direct callers need. Private Runtime, journal, lease and approval capabilities remain private.

The default-off App Server `test-support` feature reuses Engine's existing environment owner, and the TUI enables it only for dev builds. Existing tests move or stay with the owner they actually exercise; no cross-crate path mount or copied fixture store remains. Build identity delegates to the existing build-support crate. Package assets and release ordering follow the moved source owner. Native builds, existing focused checks and ordinary local transport/summary observations must be run against one frozen input set; compiler and package proof are still required. At most two native builds run at once under the existing build lock.

Preserved prerequisites and remaining gates:

- Completed prerequisite: the existing `owner_work` and
  `RuntimeControlFrontend` live in `codewhale_runtime::runtime_control`, with
  compatibility aliases in App Server. Shared protected socket layout lives in
  `codewhale_paths::daemon_socket`; Sandbox no longer depends on App Server.
  Locked Cargo metadata for all 48 workspace members is acyclic, including
  optional and target-specific production edges. The focused native socket
  checks passed; Windows transport qualification remains separate.
- Completed prerequisite: the unchanged avatar data contract lives in
  `codewhale_protocol::avatar`; the terminal kit reexports those same types.
  Engine no longer directly depends on the kit or its artwork. Engine still
  has direct ratatui/crossterm and Palette coupling, so this is not a
  terminal-free Engine claim. Protocol 0.10.3 publication/registry proof must
  precede the separately versioned kit 0.2.0 and dependent family publication.
- Publishing `codewhale-engine` to crates.io remains a separate founder action.

The blocking-call guard also needed a scope correction: an external
`#[cfg(test)] mod tests;` declaration previously hid later production code.
The corrected counts match the pre-move sources in every increased file;
the budget records existing debt, including 35 filesystem and four
canonicalization sites in TUI `lib.rs`. This move adds none of those sites.

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

Historical planning checker: `upward_edges=1 upward_refs=73`, all `engine-tools → engine`; this is not a current measured result. The Stage B map now assigns protocol parity with its defining Event/Op types, runtime API review with App Server, and both archive modules with the lower tools band. Stage B source movement and its proof remain open:
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

- The planning table's "Lines incl. tests" estimates predate extraction and
  omitted about 146k lines from the original TUI unit map. The direct source
  counts above include every Rust source file under each current crate root.
- The checker resolves paths textually. Second-hop names, glob re-exports,
  macros and `#[path]` includes are still not followed, so zero upward refs
  is necessary, not sufficient; `cargo check` is the authority.
- `codewhale-engine` is now the extracted crate. `codewhale-engine-tools`
  remains a proposed name; `crates/core` and `crates/tools` already exist
  with other content.
