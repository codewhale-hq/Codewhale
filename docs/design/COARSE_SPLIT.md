# Coarse split of codewhale-tui — 2026-10-31

Status: proposal. Founder, 2026-10-31: make the top line of crates ~400–500k
lines, like Codex and Grok Build, instead of going straight to ~10k crates.
Measured on `f510a59d94` with `scripts/split/crate_map.py`, using the same
units as [CRATE_MAP.md](CRATE_MAP.md).

## Why coarse first

| Repo | Rust packages | Largest crates (lines incl. tests) |
| --- | ---: | --- |
| Codex `codex-rs` (2026-10-19) | 162 | `core` 463k, `tui` 463k, `app-server` 197k |
| Grok Build (2026-09-29) | 104 | `xai-grok-pager` 551k, `xai-grok-shell` 430k, `xai-grok-tools` 165k |
| Codewhale today | 51 | `tui` 1.18M (570k production) |

The 66-crate map needs 324 upward refs fixed before extraction, and a
read-only review (Codex CLI, `gpt-6.1-sol`) found more it cannot see: inherent
`impl Event`/`impl Op` split from their types (`core/protocol_parity.rs:1306,
1314`), reciprocal glob coupling between `tui/ui/overlays.rs` and
`tui/ui/handlers.rs`, a lexical `cached_command_list!` macro, `#[path]` mounts,
`pub(super)` fields, `build.rs` env vars and embedded assets. Almost all of
that lives *inside* a band. Cutting only at the band boundaries avoids it.

## Stage A: three crates (recommended first)

Map: [`crate-map-coarse.toml`](crate-map-coarse.toml).

| Crate | Content (CRATE_MAP bands) | Production lines | Lines incl. tests (est.) |
| --- | --- | ---: | ---: |
| `codewhale-engine` (new) | runtime band: engine and the one turn loop, tools, agents, context and `BASE_PROMPT`, session store, model client/catalog, MCP, extension host, fleet, tasks | 294,269 | ~600k |
| `codewhale-app-server` (existing, 16k today) | host band: runtime thread manager, runtime API and routes, ACP/MCP/exec hosts, chat relay, doctor | 56,666 | ~85k |
| `codewhale-tui` (what remains) | TUI band, slash commands and the `lib.rs` composition root with `tools/registry` | 219,602 | ~420k |

The existing direction already fits: `codewhale-tui` depends on
`codewhale-app-server`, never the reverse. After stage A:
`engine ← app-server ← tui ← cli`.

Checker: `upward_edges=3 upward_refs=35 oversize=0 unassigned=0 duplicates=0`.
The whole work list:

1. **engine → tui, 28 refs.**
   - `tools/registry` (14 refs). `ToolRegistry`, `ToolRegistryBuilder`,
     `enforce_tool_authority`, `mcp_result_to_bounded_rich_tool_result`,
     `readonly_evidence_tool*` and `override_refusal_notice` are runtime
     machinery the engine and children call. Move the registry machinery into
     the engine; keep only the concrete list of tools the product registers in
     the composition root, handed to the engine as a factory. (Same finding as
     the review's #2.)
   - `crate::` root items (14 refs) from `extension_host/*`, `client`,
     `core/engine/turn_loop`: re-exports or helpers `lib.rs` exposes. Point each
     at its real owner inside the engine band.
2. **app-server → tui, 4 refs.** `doctor_fix` uses `McpServerDoctorStatus`,
   `doctor_check_mcp_server`, `default_tools_dir`, `default_plugins_dir` from
   `lib.rs`. Move those four into `doctor` or the engine.
3. **engine → app-server, 3 refs.** `profile_constitution` reads
   `runtime_api::runtime_account_api_base`; `receipts` reads
   `RuntimeThreadManagerConfig`. Move the constant and the config record into
   the engine.

Then: one `git mv` of the engine band into `crates/engine`, one of the host
band into `crates/app-server/src`, path rewrites (`crate::` → crate names),
Cargo manifests, `build.rs` env vars and feature forwarding, embedded assets
(review #12–13), and the existing guard tests (`single_turn_loop` and others)
moved with their code. Each move is proven with `cargo check` on the two
crates and the existing guard tests; at most two native builds run at once.

Parallelism: the three fixes touch disjoint files and can go to three agents;
the two moves are sequential (engine first) but each is one agent's job.

## Stage B: split the engine band near 500k

Stage A's engine is ~600k lines with tests, above the 400–500k line. Stage B
moves the lower half of the runtime band (CRATE_MAP layers 1–17: MCP, model
catalog and client, billing, route, tasks, fleet, tool contract and runtime,
tool families, context, authority, agents, session store, extension-host
dependants) into `codewhale-engine-tools`, leaving the engine, turn support,
extension host, child coordination and web tools in `codewhale-engine`.

Map: [`crate-map-coarse-split.toml`](crate-map-coarse-split.toml).

| Crate | Production lines |
| --- | ---: |
| `codewhale-engine-tools` | 225,197 (~450k incl. tests, est.) |
| `codewhale-engine` | 69,072 |
| `codewhale-app-server` | 56,666 |
| `codewhale-tui` | 202,678 |
| `cli` remainder of `lib.rs` | 16,924 |

Checker: `upward_edges=6 upward_refs=96`. The extra 60 refs are the
tools → engine edge: `HumanDecision`/`LivePosture`/`LiveRuntimeAuthority`
(CRATE_MAP P1, and the approval-authority design question in review #7), the
RLM caller (`RlmMode`, `CapturedRlmCaller`), and `ExtensionHostManager` /
`StockOperation` reached by tools, hooks, MCP and fleet (CRATE_MAP P3). Those
are the same packets the fine map needs anyway, so stage B is the first step
of CRATE_MAP rather than a detour.

## After that

Further splits happen inside one coarse crate at a time, where churn or
build time says they pay: CRATE_MAP's runtime packets inside the engine
crates, its T packets inside the TUI. The review's findings become the
entry criteria for those splits.

## Known limitations

- "Lines incl. tests" is estimated by assigning each source file to its unit;
  about 146k lines of `crates/tui/src` did not map to a unit and are left out
  of the per-crate figures.
- The checker resolves paths textually (see `scripts/split/crate_map.py`);
  grouped imports and globs that cross a band are uncounted, so the real work
  list is at least 35 refs. `cargo check` is the authority.
- No code has moved. Names (`codewhale-engine`, `codewhale-engine-tools`) are
  placeholders; `crates/core` and `crates/tools` already exist with other
  content.
