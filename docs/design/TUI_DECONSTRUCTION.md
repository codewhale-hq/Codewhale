# TUI deconstruction: crates as agent work partitions

The deliverable is a workspace where one agent, possibly sandboxed, can be
handed one crate plus the contracts it depends on, change it, prove it with that
crate's own tests, and return a patch that cannot collide with any other
agent's patch. The terminal UI becomes one client of a headless runtime along
the way, but the organizing question for every boundary is: **where can an
agent work without negatively affecting another agent?**

A lower `crates/tui` line count alone does not establish the split. A crate
boundary counts when it lets an agent build, test and land work without the
rest of the workspace.

This document defines the decomposition target. [CW-6129](https://linear.app/shannonlabs/issue/CW-6129)
and its existing children own current source status, claims and acceptance;
[the roadmap](DECONSTRUCTION_ROADMAP.md) orders structural dependencies. The
complete program remains in 0.10.3. Dated audits below are rationale, not current
assignments or permission to create another checkout or build cache.

The terminal UI becomes a presentation adapter over the existing headless
runtime. A boundary counts when it moves real ownership, migrates every
consumer and reduces dependency/rebuild closure. Moving a monolith or adding a
facade does not establish that result. Aim around 10,000 production lines per
crate and complete the existing 50,000 ceiling across the resulting workspace.
Production lines, physical lines, SCCs and build timings measure different things.

## Audited baseline (2026-10-09, `bd2a7b49c`)

Physical Rust lines, tests included. These are ownership clues, not production
LOC. Regenerate before relying on them.

| Source | Lines |
| --- | ---: |
| Whole workspace (29 crates) | ~1.47M |
| `crates/tui` | 1,280,430 in 1,077 files (~87%) |
| of which test code in test files / test dirs | ~331k |
| `crates/tui/src/tui` (terminal UI proper) | 314,291 |
| `crates/tui/src/tools` | 204,115 |
| `crates/tui/src/core` (engine, turn loop) | 89,384 |
| `crates/tui/src/commands` | 71,629 |
| `crates/tui/src/lib.rs` + `lib/` | 24,827 |
| Every other crate | under 54k each; most under 10k |

Crates named for a subsystem whose implementation still lives in the TUI:
`tools` (1.6k; the implementations are `tui/src/tools`, 204k), `mcp` (109
lines; `tui/src/mcp*` is 26k), `core` (5.8k; the engine is `tui/src/core`),
`agent` (1.6k model registry with one TUI consumer, beside the TUI's own
catalog). `cli` depends on `tui` for `run`, `route_preferences`, `config_keys`;
`tui` depends on `app-server` while the Runtime API
(`runtime_api`, 61k) lives inside `tui`.

`scripts/split/module_graph.py --report` puts the runtime closure (everything
reachable from the runtime roots without passing through UI modules) at **116
modules, about 813k lines**. A grep-based module graph (production paths,
approximate test stripping) shows the non-UI part of it as **one strongly
connected component of 85 modules**. Moving that component into one crate
would relocate the monolith, not partition it.

### Where agents collide

Commits touching `crates/` from 2026-08-10 to 2026-10-09: 3,380.

| Area | Commits | Why it collides |
| --- | ---: | --- |
| `tui/src/tui/ui` (event loop, dispatch, handlers) | 621 | one controller for every surface |
| `tui/src/core/engine` | 462 | features wire themselves into the turn loop |
| `tui/src/commands/groups` | 321 | every slash command in one tree |
| `tui/src/runtime_api` | 289 | routes, DTOs and logic in one module |
| `tui/src/tools/subagent` | 284 | |
| `crates/config` + `tui/src/config` | 283 + 199 | each feature adds fields to one schema |
| `tui/src/tui/app` | 275 | |
| `tui/src/lib.rs` | 270 | `mod` list plus composition root |

Central registration lists cause much of this: `tools/registry.rs` has one
`with_*_tools()` builder per tool family, `config.rs` (13k) holds every
feature's fields, `lib.rs` lists every module, `commands/groups/mod.rs` lists
every group. Adding a feature edits all of them.

### Reference workspaces

Local snapshots, inspected 2026-10-09; not claims about current upstream.

| | Crates | Lines | Largest |
| --- | ---: | ---: | --- |
| Codex `codex-rs` (`bdda5da56c`, 2026-07-30) | 126 | 1.23M | core 294k, tui 235k, app-server 126k |
| Grok Build (`500129c`, 2026-07-29) | 76 | 1.46M | pager 462k, shell 362k, tools 127k |

Patterns adopted here:

- **Contract crate apart from implementation crate** (Grok):
  `xai-tool-types` -> `xai-tool-protocol` -> `xai-grok-tools-api` ahead of the
  heavy `xai-grok-tools`; likewise `config-types`, `workspace-types`,
  `sampling-types`. `tools-api` exists so "host services ... must not depend on
  the tools implementation crate."
- **Feature crates that plug in through contributor traits** (Codex `ext/`):
  `extension-api` defines `ToolContributor`, `ConfigContributor`,
  `ContextContributor`, `ThreadLifecycleContributor` and others; goal,
  memories, web-search, skills, MCP, guardian and image generation are each a
  crate.
- **Tiny utility crates** (Codex `utils/*`: `output-truncation`, `pty`,
  `path`, `string`, `sleep-inhibitor`).

Patterns rejected, because each defeats agent isolation:

- Codex `core` depends on 65 workspace crates, including three `ext` crates
  that themselves depend on `core`. The hub remains; every feature agent still
  rebuilds it.
- Grok's `xai-grok-tools` implementation crate has 13 dependent crates, so a
  tool edit rebuilds pager, shell and agent.
- Both keep 300k+ line crates. Crate count is not the goal; the rules below are.

## Rules

1. **Feature crates are leaves.** Nothing depends on a feature or adapter
   implementation crate except the composition root (`codewhale-cli`, or a
   thin host crate it owns). An edit there rebuilds that crate and the final
   binary, never a sibling.
2. **Contract crates are small, logic-free and rarely changed.** They carry
   the high fan-in, so a contract edit is the one change that affects other
   agents. Contract changes are their own slices, serialized, and each lists
   the dependents it rebuilt.
3. **No central lists.** Features register through contributor traits, and
   the composition root names each crate once; adding a feature is one
   appended line there. Adopting the contributor traits deletes the
   `with_*_tools` builders, the per-feature `config.rs` fields and the command
   group lists in the same migration. Do not run both systems.
4. **Each crate owns its tests.** Unit tests move with their code; a crate's
   tests build with only that crate's dependency closure. Cross-crate behavior
   is tested at the host level.
5. **Claims name exact paths and shared compiler inputs.** The target is a
   crate that can be owned independently. Until a large crate is split,
   coordinate disjoint files and one frozen build batch instead of creating
   another checkout.
6. **Dependencies point down only** (tiers below). Cargo enforces it; an
   upward import is a compile error, not a ratchet count.

## Future sandbox qualification

Small dependency closures should eventually permit a confined agent to change
one owner while consuming read-only contracts. That is a product qualification
target, not a separate local development workflow or authorization to create
cloud resources. Preserve path confinement, cancellation, credential boundaries
and the existing Engine authority in any such journey.

Current work uses the canonical checkout on main, exact path claims and one
frozen batch per shared native input set. Use the existing shared build cache
and two-slot lock. Do not create sandbox copies, per-agent target directories,
worktrees or alternate integration lanes to bypass a conflict or build owner.
Contracts, workspace manifests, Cargo.lock and composition changes require the
same coordinated source review as their consumers. Run focused existing checks
locally; no new tests or code comments are implied by this design.

## Target map

The unit-by-unit assignment, with layers, the wave-1 work list and the wave-2
extraction rounds, is [CRATE_MAP.md](CRATE_MAP.md) and its checkable
[`crate-map.toml`](crate-map.toml) (2026-10-31). Where the two disagree, that
map is newer; its decision D8 lists the differences. The protocol crate this
plan called `runtime-wire` is named `app-server-protocol` (founder, App Server
v2, 2026-10-31).

Names are package names (`codewhale-` prefix omitted). Sizes are today's
source for those modules, tests included, measured at `bd2a7b49c`. Existing
crates are reused where their current content already fits the role.

Every crate below `services` is a leaf in the sense of rule 1 unless it is a
contract. New crates live under grouped directories:
`crates/{contracts,util,services,providers,runtime,features,tui,hosts,test}/`.
Existing crates move into those directories in one rename-only change.
The map is a dated destination sketch: the founder's 2026-10-25 S7=(c)
decision below places the TUI configuration and OAuth cluster in the
existing `codewhale-runtime`, superseding the `config` and `credentials`
rows for P3. Every eventual crate targets about 10k production lines and
stays below 50k.

### Contracts (`crates/contracts/`)

| Crate | Role | Source |
| --- | --- | --- |
| `protocol` | shared wire and domain records | exists |
| `core` | request construction, journal, ids, fragments, prefix cache | exists (5.8k) |
| `models` | provider wire DTOs and the model-client trait | exists; trait added |
| `tools` | tool contract: `ToolResult`, `ToolError`, `ToolCapability`, specs, plus capability ports (spawn agent, schedule work, work graph) that tools call and the runtime implements | exists (1.6k); ports added |
| `ext` | contributor traits: tool, config section, context/prompt fragment, command, thread lifecycle, approval review | new; replaces the central lists |
| `config-types` | config schema and route identity, no I/O | split from `config` (53k) |
| `command-contract` | portable command metadata and results | exists |
| `app-server-protocol` | Runtime API DTOs and the in-process `Op`/`Event` surface clients use | from `runtime_api` and `core/events.rs` |

### Utilities (`crates/util/`)

`paths`, `sanitize`, `release`, `build-support`, `localization` (exist);
`util-truncate` (from `tools::truncate`); `util-fslock` (from
`runtime_threads::try_lock_file_exclusive`); `util-hash` (`fast_hash`,
`hashing`, `regex_cache`, `safe_label`, now in `crates/runtime`);
`util-schema` (`tools::schema_sanitize`, 3k); `util-pty`.

### Services (`crates/services/`)

| Crate | Source | Lines |
| --- | --- | ---: |
| `config` | loading, persistence, keys (`config`, `settings`, `config_persistence`, `config_keys`) | 41k + crate |
| `execpolicy` | exists; absorbs `core::authority::auto_review` and `tool_matches_any_rule` | 13k + moved |
| `secrets` | exists | 3k |
| `credentials` | `oauth`, `credentials` | 11k |
| `state` | exists; absorbs checkpoint records from `runtime_handoff` | 4k + moved |
| `session-store` | `session_manager`, `runtime_threads` store, `session_*` | 59k |
| `sandbox` | `sandbox` | 7k |
| `billing` | `pricing`, `route_billing`, `cost_status` | 15k |
| `telemetry` | exists | 5k |

### Providers (`crates/providers/`)

| Crate | Source | Lines |
| --- | --- | ---: |
| `model-client` | shared dispatch, streaming, retry (`llm_client`, `client` core) | ~25k |
| `provider-openai-compat` | `client/chat*` | ~10k + tests |
| `provider-anthropic` | `client/anthropic.rs` | ~2.5k + tests |
| `provider-responses` | `client/responses*` | ~3k + tests |
| `provider-local` | `local_ollama` | small |
| `model-catalog` | live catalog, routing, inventory, receipts, tuning; absorbs `agent` | 24k |

### Runtime (`crates/runtime/`)

| Crate | Source | Lines |
| --- | --- | ---: |
| `engine` | `Engine::run_turn` and the tool executor only | 89k today; shrinks as contributors leave |
| `context` | `compaction`, `prompts`, `project_context*`, `runtime_handoff`, `request_manifest`, `prompt_zones`, `context_budget` | 21k + moved |
| `approvals` | `core::authority` minus policy data | ~5k |
| `agents` | `tools/subagent`, `fleet`, `agent_roster`, `worker_profile` | 107k; split again by its own seams |
| `tasks` | `task_manager`, `automation_manager`, `work_graph` | 22k |
| `runtime` | the session host composing the above; implements the `tools` ports | exists (8k) |

`BASE_PROMPT` and `Engine::run_turn` keep their single-owner contracts: one
base prompt in `context`, one turn loop in `engine`. `agents` adapts children
to that loop; it does not own another one.

### Features (`crates/features/`, all leaves)

| Crate | Source | Lines |
| --- | --- | ---: |
| `tools-fs` | `tools/file`, `apply_patch` | 12k |
| `tools-shell` | `tools/shell`, `shell_dispatcher` | 16k |
| `tools-web` | `tools/web`, `web_search`, `web_run` | 17k |
| `tools-github` | `tools/github` | 4k |
| `tools-review` | `tools/review` | 3k |
| `goal` | `tools/goal`, `operate`, `goal_loop` | 5k + moved |
| `workflow`, `workflow-js` | exist; absorb `tools/workflow` (16k) | |
| `memory` | exists; absorbs `native_memory`, `tools/remember` | |
| `mcp` | exists; absorbs the TUI MCP pool | 26k |
| `skills` | `skills` | 14k |
| `plugins` | `plugins` | 28k |
| `extension-host` | `extension_host` | 31k |
| `hooks` | exists; absorbs `tui/src/hooks` | 9k |
| `lsp` | `lsp` | 4k |
| `rlm` | `rlm`, `repl` | 5k |
| `snapshot` | `snapshot` | 6k |
| `voice`, `vision` | `voice`; `vision`, `image_attach` | 1k, 3k |
| `remote-control` | `remote_control`, `runtime_chat_relay`, `remote_setup` | 13k |
| `integrations` | `integrations` | 5k |

Each feature contributes its tools, config section, slash-command logic and
context fragments through `ext`. Its terminal presentation, if any, is a view
crate in the TUI tier.

### TUI (`crates/tui/`)

| Crate | Source | Lines |
| --- | --- | ---: |
| `tui-kit` | widgets, markdown render, textarea, mouse, `palette`, the view trait | 30k + palette |
| `tui` | event loop and app-state reducer (`tui/ui`, `tui/app`) | 97k; split reducer from render first |
| `tui-view-transcript` | `tui/history` | 12k |
| `tui-view-work` | `tui/work_surface` | 13k |
| `tui-view-pickers` | `provider_picker`, `model_picker` | 17k |
| `tui-view-setup` | `tui/setup` | 9k |
| `tui-view-approval` | `tui/approval` | 6k |
| `tui-view-hotbar` | `tui/hotbar` | 4k |
| `tui-view-ambient` | `pet_watch`, `underwater`, `ambient_life` | 13k |
| remaining views | `tui/views` (35k), split by surface | |
| `tui-commands` | slash-command presentation (`commands`, 72k) | |

The TUI depends on `app-server-protocol`, never on `runtime`, `engine` or a feature
crate. A TUI agent's sandbox therefore holds no runtime code. `cli` connects
the TUI to the runtime in process through the existing `EngineHandle`/`Op`/
`Event` seam; HTTP stays a transport for external clients.

### Hosts and test support

| Crate | Source |
| --- | --- |
| `app-server` | exists; absorbs `runtime_api`, `acp_server`, `mcp_server` (66k) |
| `cli` | the only composition root; absorbs `lib.rs::run`, `RuntimeOptions`, `route_preferences` and the wiring list (45k today in `lib.rs` + `lib/`) |
| `test-support` | mock provider, fixtures, `test_support` |
| `pty-harness`, `tui-goldens` | PTY and golden-frame tests |

About 70 crates. That is a consequence of the rules, not a target; split
further wherever a crate's churn still makes agents collide (`agents`,
`engine` and `tui` are the known candidates).

## Breaking the current cycle

The 85-module cycle is held together mostly by **types and pure functions in
the wrong module**, not by behavioral dependencies. Heaviest back-edges
(grep-based counts; a few are test-only references):

| Back-edge (refs) | Referenced | Destination |
| --- | --- | --- |
| `tools -> fleet` (62) | `AgentProfile`, `ChildAuthority`, `FleetRoster` | profile/authority data to a contract; behavior stays in `agents` |
| `tools -> core` (55) | `tool_catalog::enforce_tool_denial`, `tool_matches_any_rule` | `execpolicy` |
| `config -> core` (31) | `AutoReviewPolicy`, `AutoReviewAction`, `StepBudgetSource` | `execpolicy`; budget type to `core` |
| `client -> tools` (27) | `truncate` spillover, `schema_sanitize` | `util-truncate`, `util-schema` |
| `client -> compaction` (14), `client -> runtime_handoff` (10) | checkpoint and summary builders | `core` / `state` |
| `mcp -> core` (12) | `HumanDecision`, rule matching | `protocol` / `execpolicy` |
| `session_manager -> runtime_threads` (11) | `try_lock_file_exclusive`, thread store | `util-fslock`, `session-store` |

Moving these down is the same work as creating the contract and utility tiers.

## Sequence

Every move is rename-only (`git mv` plus path updates) with one wiring line,
so in-flight work rebases across it. Behavioral changes land separately.

0. **Measure.** Add a closure report to `scripts/split/`: for each crate, the
   lines in its dependency closure and in its reverse closure, plus a
   generator for a minimal sandbox workspace. Record the baseline. Extend
   `module_graph.py` with the tier table so in-TUI back-edges ratchet down
   before their crates exist.
1. **Contracts and utilities.** Create `ext`, `config-types`, `app-server-protocol`,
   the `util-*` crates, and the `execpolicy`/`state`/`core` absorptions. This
   cuts the back-edges above.
2. **Contributor registration.** Migrate every `with_*_tools` builder,
   per-feature config field and command group list to `ext` contributors
   wired in `cli`, in one slice per list, deleting the old list in that slice.
3. **Pilot leaf: `tools-web`.** Extract it, then run one sandboxed agent
   against it end to end. Record payload size, cold and warm build time, test
   result and whether the patch applied cleanly. Adjust this document from the
   result before extracting more.
4. **Remaining features**, least coupled and most churned first.
5. **Providers**, one adapter per crate.
6. **Runtime tier and hosts.** `engine`, `context`, `approvals`, `agents`,
   `tasks`, `session-store`; move `runtime_api` into `app-server`; move the
   composition root into `cli` and delete the `lib.rs` path alias.
7. **TUI.** Separate the app-state reducer from rendering, then extract views.
   Last, because it is the most entangled and benefits from every earlier
   contract.

The runtime -> UI ratchet (`scripts/split/module_graph.py`,
`scripts/runtime-boundary-baseline.json`, enforced by
`scripts/check-command-crate-boundaries.py`) stays the gate until the runtime
tier is out of `crates/tui`; then Cargo enforces the boundary and the ratchet
is deleted. The 0.10.2 X2 snapshot stands at 0 production references and
30 test references, down from 11 and 32. The RS-11 roster split then removes
five test references and the final tools/crossterm test import, leaving 25
test references and no UI-library imports. The following tools split moves the
Workflow panel assertions to its existing history test and removes duplicated
coordination pager checks, leaving 18 test references. The history split moves
persistence/render tests to history and uses the runtime MCP decoder in the
engine, leaving 13 test references. Moving the two unchanged App fixtures to
their UI owner and migrating all 114 consumers leaves 6 test references.
Moving redaction decisions onto Config, workspace trust into config, and the
unchanged constitution checkpoint into prompts removes the final 6. RS-11
ends at production 0, test 0, late 1, UI-library 0 and doc 0. The late
engine/exec-agent test reference belongs to a later slice. The current baseline
is the authority.
`crates/runtime` must never depend on `tui`, `cli`, ratatui, crossterm,
`ansi-to-tui` or a terminal component kit; the runtime reaches the terminal
only through `host_terminal`.

## Operational plan: assignable slices

Re-measured for X2 on 2026-10-12 with `python3 scripts/split/partition_report.py` (per-module
waves, sizes and blockers) and `--scc-edges 60` (the heaviest edges inside the
cycle). Regenerate before assigning; the numbers below are a snapshot.

- Before X2 the largest runtime cycle had **98 modules**; after X2 it has **82**.
  with `tools` 152k, `core` 50k, `fleet` 39k, `runtime_api` 31k, `client` 26k
  production lines in the earlier size snapshot. X2 removes all 11 production
  back-edges (`runtime_api -> commands` 9, `tools -> commands` 2); `tui`,
  `commands` and `lib.rs` leave the largest cycle. These are source graph
  measurements, not a claim that the runtime crate migration has shipped.
- 30 modules (about 17k production lines) are **wave 0**: nothing they depend on
  is still waiting to move. They are claimable as crates now.
- Moving the cycle as a unit is the rejected pattern (see Rules). The open runtime
  split issue that moves the whole core into `crates/runtime` in one change
  (RS-14, #6941) conflicts with the Rules above and is replaced by the slices
  below unless the founder decides otherwise; it should not start before that
  decision.

Each slice below was proposed by one analyst and challenged by another that
re-read the code. "Held" means the challenge confirmed it. The corrections are
part of the slice.

| Slice | Claim unit (paths) | Size | Status of the analysis | Must happen first | Result |
| --- | --- | --- | --- | --- | --- |
| **X1 util-fslock** | `crates/fslock/` (new), `runtime_threads.rs`, `session_manager.rs`, `session_reconcile.rs`, `operate.rs` | S, ~190 lines | **Held.** Five call-site clusters in `runtime_threads.rs` (incl. `has_live_holder`), `session_manager.rs:1954` is production | none | removes the `session_manager <-> runtime_threads` lock edge; first leaf for `session-store` |
| **X2 command catalog port** | `crates/tui/src/command_catalog.rs`, `commands/mod.rs`, `runtime_api.rs` list handler, `tools/tui_help.rs`, `extension_host/command.rs` | M | **Landed in 0.10.2.** One host port supplies all three consumers; the Runtime API test server captures its thread-local guard | none | ratchet `prod` 11 -> 0, `test` 32 -> 30, largest cycle 98 -> 82 |
| **X3 tool contract, part 1** | `crates/execpolicy/src/canonical_action.rs`, `crates/tools/src/lib.rs`, `crates/tools/src/param_alias.rs`, `crates/tools/src/patch_preflight.rs`, `crates/tui/src/tools/mod.rs` | M, no `pub use` shim (callers rewired) | Direction right; **not rename-only** (visibility widening, split test module) | none | first tool-contract move; unblocks TUI views depending on a crate, not the runtime |
| **X4 util-schema** | `crates/util-schema/` (new), `tools/schema_sanitize.rs`, `schema_canonicalize.rs` | M | Needs `prepare_tools_for_strict_mode` moved next to its caller first (it is the only use of `codewhale_models::Tool`) | X3 not required | removes `client -> tools` schema refs |
| **X5 config-types, part 1** | `crates/config-types/` (new), `features.rs`, `route_receipt.rs`, `plugins/types.rs` | S-M, ~400 lines | **Corrected:** move `impl Display for PluginId` with `PluginId` (orphan rule); leave out `CustomLspDef` (already in `crates/config`); the `lsp` and `plugins` edges stay | none | removes the type-only up-edges from `config`/`settings` |
| **X6 app-server-protocol (was runtime-wire), part 1** | `crates/app-server-protocol/` (new), `core/events.rs`, `core/ops.rs`, `rlm/bridge.rs` | S, ~300 lines | **Corrected:** `ModelToolCall` stays in `core` (moving it makes a Cargo cycle `models -> config -> execpolicy`); widen `is_bidi_format_control` and `NESTED_RLM_STATUS_PREFIX` | none | first DTOs a TUI sandbox can depend on without `core` |
| **X7 tool rules into execpolicy** | `crates/execpolicy/src/tool_rules.rs`, `core/engine/tool_catalog.rs` | S | Part of the fleet plan; `tool_denied`, `tool_matches_any_rule`, `requires_raw_shell`, `policy_tool_aliases` | none | cuts `tools -> core` and `config -> core` rule references |
| **X8 agent-contract** | `crates/agent-contract/` (new), `fleet/role.rs`, `fleet/worker_profile.rs`, `tools/subagent/mod.rs` | M | **Corrected:** the inherent `impl FleetRole` at `tools/subagent/mod.rs:639` becomes illegal after the move; convert to free functions first; resolve the two different `ChildAuthority` types | X7 | removes `tools -> fleet` (62 refs) |
| **X9 sandbox pilot** | `crates/sandbox/` (new), `tui/src/sandbox/` | S then M | **Superseded by S17:** sandbox and `shell_dispatcher` move together; the integration `#[path]` include is removed and the `cfg(test)` env-lock branch is preserved under the `test-support` feature. The config/oauth edges resolve through the existing `codewhale-runtime` dependency; no edge cut is needed | none | second feature leaf after `tools-web` |
| **W0-S1 runtime residents** | `crates/runtime/` ← `features`, `logging`, `startup_trace`, `resource_telemetry`, `runtime_policy`, `lane_control` | S, landed 2026-10-10 (8aea0265c) | **Landed.** `python3 scripts/split/move-modules.py` dry run is read-only and safe | none | six fewer tui modules; no `test_support` needed |
| **W0-S2.. test-support and the rest of wave 0** | the existing `codewhale-runtime` `test-support` feature, then remaining leaves | M each | **Environment helpers moved in 0.10.2:** one process environment lock in the Runtime test-support owner; no new test-support crate | W0-S1 | the TUI shim retains its resolver fences; App fixtures live in the UI owner; remaining residents follow RS-10 |
| **B billing/catalog** | new crate above `codewhale-runtime` ← `cost_status`, `pricing`, `route_billing`, `provider_catalog_live`, `provider_lake`, `models_dev_live`, `codex_model_cache`, `reasoning_preference` | M, 11,091 strict production lines (24,352 file-level; `partition_report.py`'s per-module figure is file-level) | **Re-measured 2026-10-31 on `1b0e51005b`.** The eight modules' only production edges into the 59-module cycle are 6 refs into `client` (`CodewhaleClient::for_catalog_refresh` in `provider_lake` and `codex_model_cache`, `client::ollama_catalog::load_model`, `client::AvailableModel`) and 1 into `provider_readiness`. 590 inbound production refs from the TUI follow the move. Tests reach out 176 times, 149 of them to `test_support`. Every module uses `codewhale_runtime::config`, so the crate depends on the runtime crate; the earlier "must not depend on the runtime tier" constraint predates `Config` moving there. Not rename-only: catalog refresh stays beside `client` (split `provider_lake`/`codex_model_cache` into store and refresh driver) | the refresh-driver split; open PRs that touch `session_manager.rs` or other `pricing`/`cost_status` callers merged first | measured, not started |
| **S0 closure report** | `scripts/split/closure_report.py` | S (report), M (sandbox generator) | Held as a tool; deletes no edges | decide where sandbox third-party sources come from | the numbers the sandbox contract needs |

Running them in parallel:

- Claim units are disjoint across X1, X3, X5, X6, X7 and W0-S1, so those can be
  owned by six agents at once.
- Every new crate edits files only an integrator may touch: root `Cargo.toml`,
  `Cargo.lock`, `crates/tui/Cargo.toml`, `scripts/release/crates.sh`,
  `scripts/dev-test.sh`. Serialize those edits through one integrator; new
  package names are a release-time human gate.
- Each slice holds one of the machine's two native build slots through
  `scripts/dev-cargo.sh` and `scripts/build-lock.py` (founder, 2026-10-25).
  Freeze shared compiler inputs with their owners before a build; do not
  create another checkout or target directory to bypass contention.
- Each slice is verified by `cargo check` on both crates, the crate's own
  tests, `python3 scripts/split/module_graph.py --check` with the counts it
  should lower, and `python3 scripts/split/partition_report.py` before and after.

Mapping of the open issues: RS-8 (#6935), RS-9 (#6936), RS-12 (#6939) and
RS-13 (#6940, first sub-slice) landed; X2 completes RS-13; W0-S1 and W0-S2 replace
RS-10 (#6937) and the test-support half of RS-11 (#6938); RS-14 (#6941) is
replaced by this table pending the founder's decision.

## Execution and current ownership

Canonical source and the dated receipts on CW-6129 establish what has landed.
S35 thread ownership, S52 terminal helpers, N13 error taxonomy, pure model
pricing and native skill ownership have moved with their consumers. Runtime
configuration now lives in `crates/runtime/src/config.rs`; do not recreate the
removed TUI config owner. Skill listing and bundled policy share the existing
registry/configuration rather than separate prompt or activation stores.
These moves do not complete the target: the TUI remains far above 50,000
production lines and the remaining runtime/UI ownership cycle must be cut.

Continue from the current dependency graph and owning children, in this order:

1. Preserve integrity, trust boundaries, durable writes and cancellation while
   cutting actual contract/configuration ownership edges. Read the existing
   runtime and config owners before proposing another abstraction.
2. Move feature and provider implementations with their final callers and
   existing checks. Preserve reviewed plugin authority, tool permission
   ceilings, credentials, typed human decisions and model-visible receipts.
   The remaining MCP/DSH, durable Tasks, isolated UI and gix work stays in 0.10.3.
3. Complete Engine, agents, sessions, runtime API and tool ownership without a
   second turn loop, event authority or session store. Keep the runtime API
   separate from transport-only app-server routing; avoid another large hub.
4. Finish the terminal reducer/render boundary and make the CLI the composition
   root. Remove superseded shims only after the last consumer migrates.
5. Measure every resulting crate, dependency closure and affected edit/build
   path. Complete internal splits where an ownership move still leaves an
   oversized crate. Do not claim speedups from line counts or a passing compile.

The target map above describes responsibilities, not a required new directory
layout or a second queue. Remaining RS/S/N and command/config/API children keep
their full acceptance on CW-6129. Former wave branches, fixed patch-release
ladders and the old RS-14 A/B narrative are historical proposals; none reduce
the admitted scope or authorize relocating the whole component as completion.

### Other product owners

The desktop product is `codewhale-app`'s existing native root
`codewhale-gpui`, using GPUI/weGPUI and Codewhale-owned components. It is active
product source, not legacy code to freeze or delete. The founder's October 24/29 direction selects that native host and its
component migration.
`crates/client` is the separate browser client. The superseded Tauri
`crates/desktop-host` was deleted; no extraction or packaging should target it.
Coordinate App/weGPUI changes with their current writers and retain Engine,
account, accessibility, IME, focus, drafts and locale contracts.

Platform route decomposition and shared protocol/client ownership remain with
their actual repositories and existing issues. Derive current file/crate sizes
from source rather than treating the October9/24 audit as today's inventory.
A native, browser, mobile or CU source change needs evidence for that surface;
Engine helper checks alone do not qualify it.

## Known limitations

- **Native builds share two machine build slots.** Cargo also locks each
  target directory. The isolation gain locally is smaller rebuilds. Freeze
  shared compiler inputs before compiling, and use the build-lock wrapper.
  Do not create per-agent target directories on a shared host; each one is a
  full build cache.
- **The final binary still links everything.** Relinking `codewhale` after any
  change is a fixed cost no crate split removes.
- **Contract edits still fan out.** The design makes them rare and visible,
  not free.
- **Every workspace crate publishes to crates.io**
  (`scripts/release/publish-crates.sh`, order derived by
  `validate-crate-publish-order.py`). Each new crate is a new public package
  whose name must be claimed at release. Publication remains a human gate.
- **An ownership move may leave an oversized crate.** Complete its internal
  split within the admitted program, guided by current churn and closure data;
  the initial map does not waive the 50,000 production-line ceiling.
- **No speedup has been measured.** Claims wait for the step 0 report and the
  step 3 pilot.

## Verification

- Preserve the model-facing runtime receipt, prompt and cache-prefix
  semantics, tool names and order, serialized records, and public commands
  across mechanical moves. A deliberate behavior change names and tests the
  intended difference.
- Move private unit tests with their implementation. A `#[path]` split stays
  in the same compilation unit and proves nothing about isolation. Do not make
  internals public just to relocate tests.
- Exercise mock-provider parent and child turns, approval and denial,
  streaming, cancel and steer, explicit goals, pause and resume, reconnect,
  restart recovery and terminal fan-in at each affected boundary.
- For every extracted crate, use a focused canonical Cargo invocation and
  record its actual dependency and reverse closures. Do not create another
  source checkout to demonstrate isolation. Existing tests move with ownership;
  new test functions and comments require a task-specific request.
- Measure warm edit/build/test cycles and peak memory for a provider edit,
  tool edit, renderer edit and locale edit before and after, with compiler,
  profile, features and cache state recorded.
- Local tests, full gates, hosted CI, installed artifacts and PTY behavior
  remain separate evidence. Exhaustive and hosted checks run only against the
  completed immutable candidate, with only invalidated evidence repeated.

## Model judgment and test-time compute

The founder's September 9 direction calls for model judgment about useful
planning, delegation, verification and durable continuation, including a useful
inferred objective within the user's authorized task. Ordinary multi-step work
need not create a goal. Preserve runtime-owned state, provenance, accounting,
cancellation and user controls. Goal, reasoning
and continuation policy must share the existing owners and preserve explicit
route choices and spending authority. There is no authorization here for a
classifier before every turn, another execution loop, a paid evaluation or an
automatic expansion of compute budgets.

The [dated reference audit and implementation rationale](https://github.com/codewhale-hq/Codewhale/blob/2231d73df5d6d4538653a54920695a389d575451/docs/design/TUI_DECONSTRUCTION.md#model-judgment-and-test-time-compute)
records the inspected DSH/Codex/Kimi snapshots and original acceptance scenarios.
Its statements about the then-current goal implementation must be rechecked
against the owning source; they are not current findings or assignments.
Quality, correct stopping, continuity and authorized cost matter; merely
creating a goal or taking more steps is not evidence of improvement.
