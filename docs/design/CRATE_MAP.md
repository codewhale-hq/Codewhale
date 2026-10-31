# codewhale-tui crate map

Status: proposal, 2026-10-31. The unit-by-unit assignment is [`crate-map.toml`](crate-map.toml); check it with
`python3 scripts/split/crate_map.py check docs/design/crate-map.toml`. It refines the Target map in
[TUI_DECONSTRUCTION.md](TUI_DECONSTRUCTION.md). Measured on `f510a59d94`.

The crate the deconstruction plan called `runtime-wire` is named `app-server-protocol` here, per the founder's
2026-10-31 App Server v2 decision: one protocol crate, not two.

Made by a judge-panel workflow: four architects (doc-faithful, min-cut, contracts-first, agent-partition) each
iterated a complete map against the checker; three judges (architecture, executability, parallelism) ranked them;
one synthesizer merged the winner with grafts. A separate read-only review followed.

Map: `docs/design/crate-map.toml`. Add `--json <file>` to the checker for the full edge list.

Checker command, run against canonical `/Volumes/VIXinSSD/CW/codewhale` (no repository file was edited):

```
python3 scripts/split/crate_map.py check docs/design/crate-map.toml
```

Checker summary line, verbatim:

```
summary: cycles=1 upward_edges=118 upward_refs=324 oversize=0 unassigned=0 duplicates=0
```

66 crates, 463 units, every unit assigned once, none over 50,000 lines. 118 upward crate edges carrying 324 refs are the whole work list. The checker's one cycle is the single strongly connected component those upward edges close; it disappears when the list reaches zero.

## How this map was made

Base: the top-ranked agent-partition map (324 refs). These judge grafts and violation fixes were applied, each measured with a local evaluator that reproduces the checker's numbers (it gave 324 on the base map, the same as the checker):

- session-ops merged back into session-store: one store authority (architecture judge). Measured -2 refs.
- core/protocol_parity moved from app-server-protocol to thread-host, so app-server-protocol stays a contract (doc-faithful graft). -2.
- tool_inspection moved into app-server-protocol (contracts-first placement). -2.
- tools/registry (the central tool list) moved from its own tool-catalog crate into cli, the composition root (doc-faithful graft; the architecture judge flagged tool-catalog below engine). +4. The executor helpers engine and agents read from it are work packet P4.
- New tool-runtime crate takes tools/handle, resource_admission, tool_result_retrieval, harness and dynamic out of tool-contract, so the contract carries no runtime logic (architecture judge). +1.
- integrations (the DSH scene/skin renderer, which uses ratatui) moved out of the host-band doctor crate into tui-setup. Its only users are lib.rs and tui/setup. -1. This clears the runtime/host-to-ratatui violation for integrations.
- Kept from agent-partition: command_catalog and keybinding_table in the existing command-contract crate, and context_report in commands-contract. Together these give 0 runtime-to-TUI refs, which the checker JSON confirms: no upward edge goes from a runtime or host crate into a tui-* or commands-* crate.
- Corrected from agent-partition's report. The `impl App` at commands/contract.rs:4942 is production code (command_contexts_with_config), not cfg(test); it is work item P0a. The claimed leaves were wrong: with upward edges removed, the only crate nothing depends on is cli.

Tried and rejected: the doc's feature-leaves-above-engine order costs +181 refs (505 total), and a partial version costs +46; see D1. Merging hooks-host into extension-host gave 0 gain. Moving rlm into engine (-8) and tools/subagent/engine into engine (-5) were left out because they lump work into the 28k engine; see D5. Moving commands/contract into tui-app costs +54.

Single owners hold. The one turn loop (`core/engine/turn_loop`) is in engine, BASE_PROMPT (`prompts`) is in context, the one session store (`session_manager` + `runtime_threads/<self>`) is in session-store, and the event authority (`core/events`) is in app-server-protocol.

## 1. Crates

| crate | layer | band | role | prod lines | units |
|---|---|---|---|---:|---|
| command-contract (EXISTING) | L1 | runtime | EXISTING crate: absorbs pure command and keybinding tables | 506 | `command_catalog`, `keybinding_table` |
| telemetry (EXISTING) | L1 | runtime | EXISTING crate: absorbs the telemetry notice | 390 | `telemetry_notice` |
| mcp-pool | L2 | runtime | MCP client pool and transports | 13,274 | `mcp/*` |
| model-catalog | L2 | runtime | live provider/model catalog and readiness | 8,320 | `provider_lake`, `provider_catalog_live`, `models_dev_live`, `codex_model_cache`, `model_inventory`, `provider_readiness`, `model_profile`, `model_relevance`, `reasoning_preference`, `route_receipt` |
| remote-control | L2 | runtime | remote control protocol and pairing (leaf, no crate refs) | 4,934 | `remote_control` |
| billing | L3 | runtime | usage metering, pricing and cost receipts | 5,991 | `pricing`, `route_billing`, `cost_status`, `scorecard` |
| route | L3 | runtime | route resolution, budgets and tuning | 2,713 | `route_runtime`, `route_budget`, `route_preferences`, `auto_reasoning`, `request_tuning`, `runtime_options` |
| model-contract | L4 | runtime | LlmClient trait and LlmError: the model-call contract | 1,945 | `llm_client`, `llm_error_envelope` |
| tasks | L4 | runtime | tasks, automations, work graph, operate, todo and plan state | 14,454 | `task_manager`, `automation_manager`, `work_graph`, `operate`, `tools/todo`, `tools/plan` |
| model-client | L5 | runtime | CodewhaleClient dispatch, retry, provider wire adapters and auto-routing; ~20k because impl CodewhaleClient spans client, chat, anthropic, responses and system_one | 20,105 | `client/<self>`, `client/wire`, `client/stream_entry`, `client/prepared`, `client/role_placement`, `client/system_one`, `client/reasoning_effort`, `client/deepseek_effort`, `model_routing`, `superfast`, `turn_route_plan`, `client/chat`, `client/anthropic`, `client/responses`, `client/provider_native_search` |
| fleet-roster | L6 | runtime | fleet roster, profiles and member identity | 9,327 | `fleet/*` |
| provider-local | L6 | runtime | local Ollama provider catalog | 640 | `client/ollama_catalog`, `local_ollama` |
| tool-contract | L6 | runtime | tool spec, envelopes, truncation, budgets and arg repair (logic-light contract) | 4,056 | `tools/<self>`, `tools/spec`, `tools/truncate`, `tools/execution_envelope`, `tools/arg_repair`, `tools/tool_call_budget`, `tools/large_output_router`, `tools/activity` |
| hooks-host | L7 | runtime | hook execution | 3,795 | `hooks` |
| tool-runtime | L7 | runtime | tool execution plumbing: handles, admission, result retrieval, harness, dynamic tools | 2,324 | `tools/handle`, `tools/resource_admission`, `tools/tool_result_retrieval`, `tools/harness`, `tools/dynamic` |
| tools-shell | L8 | runtime | shell and process tools | 11,853 | `tools/shell`, `tools/shell_output`, `tools/terminal_session`, `tools/test_runner`, `tools/cargo_failure_summary`, `tools/dev_server_readiness`, `tools/run_tool`, `tools/rust_format`, `tools/syntax_check`, `tools/diagnostics`, `tools/validate_data` |
| tools-fs | L9 | runtime | file, search and media (image, vision, voice) tools | 11,488 | `tools/file`, `tools/file_tool`, `tools/file_search`, `tools/search`, `tools/apply_patch`, `tools/read_media`, `tools/image_ocr`, `tools/pdf`, `tools/pandoc`, `tools/diff_format`, `tools/revert_turn`, `image_attach`, `vision`, `voice` |
| context | L10 | runtime | prompt assembly, compaction and handoff (one BASE_PROMPT) | 12,802 | `prompts`, `project_context`, `project_context_cache`, `compaction`, `runtime_handoff`, `request_manifest`, `repo_law`, `profile_constitution` |
| tools-work | L11 | runtime | planning, goal, todo and scheduling tools | 5,816 | `tools/tasks`, `tools/goal`, `tools/automation`, `tools/send_later`, `tools/notify`, `tools/session`, `tools/user_input`, `tools/tui_help`, `tools/speech`, `tools/project` |
| authority | L12 | runtime | approval authority, grants and approval log | 6,204 | `core/authority`, `approval_log`, `tools/approval_cache`, `tools/approval_summary` |
| agents | L13 | runtime | subagent tool, child records and lifecycle; ~23k because tools/subagent/mod.rs alone is 19.5k, split it by seam before going lower | 22,743 | `tools/subagent/*` |
| agent-roster | L14 | runtime | agent roster rows shared by runtime and views | 399 | `agent_roster` |
| app-server-protocol | L15 | runtime | in-process Op/Event surface, turn/session records and tool inspection DTOs | 4,230 | `core/<self>`, `core/events`, `core/ops`, `core/turn`, `core/session`, `core/runtime_contract`, `runtime_api/review`, `tool_inspection` |
| rlm | L16 | runtime | recursive language-model runtime | 1,439 | `rlm` |
| session-store | L16 | runtime | the one session store, with export, peek, resume, reconcile, purge and receipts | 18,344 | `session_manager`, `runtime_threads/<self>`, `runtime_threads/manager/history`, `runtime_threads/turn_artifacts`, `session_projection`, `session_secret_scrub`, `usage_persistence`, `todo_snapshot`, `session_export`, `session_peek`, `session_resume`, `session_reconcile`, `purge`, `receipts` |
| tools-review | L16 | runtime | review, verify and git/github tools | 13,268 | `tools/review`, `tools/review_core`, `tools/review_host`, `tools/review_hunks`, `tools/review_pr`, `tools/review_triage`, `tools/verifier`, `tools/verify`, `tools/github`, `tools/git`, `tools/git_history`, `tools/git_tool`, `git_status` |
| fleet-runtime | L17 | runtime | fleet manager, executor, scheduler and ledger | 9,690 | `fleet/manager`, `fleet/executor`, `fleet/host`, `fleet/scheduler`, `fleet/control`, `fleet/ledger`, `fleet/tailnet` |
| tools-ext | L17 | runtime | codemode, rlm, lsp, mcp, plugin and memory tools | 6,086 | `tools/codemode`, `tools/js_execution`, `tools/fim`, `tools/rlm`, `tools/lsp`, `tools/mcp_registry`, `tools/runtime_mcp`, `tools/plugin`, `tools/request_plugin_install`, `tools/skill`, `tools/remember`, `tools/native_memory` |
| tools-workflow | L17 | runtime | workflow tool | 8,061 | `tools/workflow`, `tools/workflow_plan_approval` |
| turn-support | L18 | runtime | engine-free turn helpers: tool catalog, streaming, heartbeat | 4,529 | `core/engine/tool_catalog`, `core/engine/reviewer`, `core/engine/token_estimate_cache`, `core/engine/tool_media`, `core/engine/context`, `core/engine/turn_heartbeat`, `core/engine/tool_preparation`, `core/engine/dispatch` |
| extension-host | L19 | runtime | extension host: registry, supervisor, extension MCP | 18,777 | `extension_host/*` |
| agents-coord | L20 | runtime | child coordination, child engine adapter and advisor | 6,043 | `tools/subagent/coord`, `tools/subagent/engine`, `tools/subagent/advisor` |
| tools-web | L20 | runtime | web, fetch and search tools | 11,246 | `tools/web`, `tools/web_run`, `tools/web_search`, `tools/web_tool`, `tools/fetch_url`, `tools/finance` |
| engine | L21 | runtime | the one turn loop (Engine::run_turn). Over 25k by necessity: 14 files carry inherent impl Engine blocks (turn_loop, child_host, rlm_host, tool_execution...) and Rust keeps inherent impls in the defining crate; engine-free helpers already peeled to turn-support | 28,477 | `core/engine/*` |
| thread-host | L22 | host | runtime thread manager hosting engines, plus the protocol-parity cross-check | 18,311 | `runtime_threads/manager/<self>`, `core/protocol_parity` |
| doctor | L23 | host | doctor checks and fixes | 1,384 | `doctor`, `doctor_fix` |
| runtime-api-routes | L23 | host | runtime API handler groups | 10,630 | `runtime_api/git`, `runtime_api/thread_history`, `runtime_api/sessions`, `runtime_api/computer_display`, `runtime_api/mobile`, `runtime_api/plugins`, `runtime_api/secrets`, `runtime_api/workspace` |
| runtime-api | L24 | host | runtime HTTP API core | 18,570 | `runtime_api/<self>`, `runtime_api/auth`, `runtime_api/listener`, `runtime_api/avatars`, `runtime_api/constitution`, `runtime_api/context`, `runtime_api/diagnostics`, `runtime_api/jobs`, `runtime_api/members`, `runtime_api/memory_lens`, `runtime_api/notification_delivery`, `runtime_api/plans`, `runtime_api/targets`, `runtime_api/thread_delete`, `runtime_api/voice`, `runtime_api/web`, `runtime_api/mcp_import`, `runtime_api/lsp`, `runtime_api/terminal`, `runtime_api/turn_artifacts` |
| hosts | L25 | host | acp, mcp server and exec hosts | 4,822 | `acp_server`, `mcp_server`, `exec_agent`, `eval` |
| remote-relay | L25 | host | runtime chat relay and remote setup | 2,949 | `runtime_chat_relay`, `remote_setup` |
| commands-core | L26 | tui | slash command registry and traits | 2,266 | `commands/<self>`, `commands/traits`, `commands/user_commands`, `commands/user_registry`, `commands/discovery`, `commands/groups/<self>` |
| tui-kit (EXISTING) | L26 | tui | EXISTING crate: absorbs dependency-free terminal primitives | 1,886 | `tui/color_compat`, `tui/motion`, `tui/clipboard`, `tui/frame_rate_limiter` |
| tui-chrome | L27 | tui | terminal widgets and primitives | 6,055 | `tui/focus_texture`, `tui/hover_hit`, `tui/hover_layer`, `tui/format_helpers`, `tui/selection`, `tui/key_actions`, `tui/key_shortcuts`, `tui/shell_key_routing`, `tui/tideline`, `tui/vim_mode`, `tui/paste`, `tui/output_rows_cache`, `tui/transcript_cache`, `tui/composer_chrome`, `tui/display_refresh`, `tui/footer_ui`, `tui/ocean`, `tui/ambient_life` |
| tui-widgets | L28 | tui | shared TUI widgets and cards, approval card included | 9,529 | `tui/widgets`, `tui/approval` |
| tui-state | L29 | tui | TUI state records shared by app and views | 2,675 | `tui/startup_defaults`, `tui/session_boot`, `composer_history`, `tui/mention_completion`, `tui/workspace_context`, `tui/plan_handoff`, `tui/pending_requests`, `tui/session_metrics`, `whats_new` |
| tui-transcript | L29 | tui | transcript and history views | 12,911 | `tui/history`, `tui/transcript`, `tui/live_transcript`, `tui/active_cell`, `tui/hot_tail`, `tui/pager`, `tui/redaction_gate`, `tui/gate_receipts`, `tui/backtrack` |
| tui-views | L30 | tui | core modal views | 11,367 | `tui/views/<self>`, `tui/views/help`, `tui/views/mode_picker`, `tui/views/status_picker`, `tui/views/route_save_prompt`, `tui/views/router_setup`, `tui/views/skills_manager`, `tui/views/workflows_manager`, `tui/whales` |
| tui-pickers | L31 | tui | theme, settings, feedback, session and file pickers | 4,716 | `tui/theme_picker`, `tui/settings_picker`, `tui/feedback_picker`, `tui/session_picker`, `tui/file_picker`, `tui/file_tree` |
| tui-views-ext | L31 | tui | extension and automation views | 4,935 | `tui/views/extensions`, `tui/views/automations`, `tui/automation_panel` |
| tui-app | L32 | tui | TUI app state | 13,284 | `tui/app/*`, `tui/<self>`, `tui/footer_hints`, `tui/behavioral_tips`, `tui/plugin_suggestions` |
| commands-session | L33 | tui | session commands | 3,864 | `commands/groups/session`, `commands/session_export_host`, `commands/session_group`, `commands/session_structcopy_host` |
| tui-model-pickers | L33 | tui | provider and model pickers | 10,758 | `tui/provider_picker`, `tui/model_picker` |
| tui-routing | L33 | tui | tui side routing of jobs and tools | 9,118 | `tui/tool_routing`, `tui/subagent_routing`, `tui/automation_routing`, `tui/shell_job_routing`, `tui/mcp_routing`, `tui/auto_router`, `tui/context_inspector`, `tui/notifications`, `tui/control_socket`, `tui/herdr`, `tui/window_control`, `tui/worktree_manager`, `tui/persistence_actor` |
| commands-ext | L34 | tui | plugin, skill, memory, project, utility commands | 7,045 | `commands/groups/plugins`, `commands/groups/skills`, `commands/groups/memory`, `commands/groups/project`, `commands/groups/utility` |
| tui-views-fleet | L34 | tui | fleet views | 6,079 | `tui/views/fleet_detail`, `tui/views/fleet_list`, `tui/views/fleet_roster`, `tui/views/fleet_setup` |
| tui-work | L34 | tui | work surface, hotbar and agent rails | 12,919 | `tui/work_surface`, `tui/phase_strip`, `tui/hotbar`, `tui/sidebar`, `tui/infoline`, `tui/background_finished`, `tui/background_indicator`, `tui/agent_focus`, `tui/agent_details`, `tui/agent_roster`, `tui/coordination_detail` |
| tui-setup | L35 | tui | setup, onboarding, startup and the DSH integration (renders with ratatui) | 12,091 | `tui/setup`, `tui/onboarding`, `tui/startup_input`, `tui/launch_resume_confirm`, `integrations` |
| tui-ambient | L36 | tui | pet, underwater and ambient life | 7,016 | `tui/pet_watch`, `tui/underwater` |
| commands-config | L37 | tui | config commands | 7,115 | `commands/config_group`, `commands/config_host`, `commands/config_main_host`, `commands/config_main_import_host`, `commands/groups/config`, `commands/debug_group` |
| tui-composer | L37 | tui | composer input, mentions and menus | 9,103 | `tui/file_mention`, `tui/git_mention`, `tui/file_frecency`, `tui/file_picker_relevance`, `tui/user_input`, `tui/composer_ui`, `tui/slash_menu`, `tui/command_palette`, `tui/context_menu`, `tui/mouse_ui`, `tui/external_editor`, `tui/prompt_suggestion`, `composer_stash` |
| tui-frame | L38 | tui | TUI frame rendering | 5,494 | `tui/ui/frame`, `tui/ui/overlays`, `tui/ui/activity_detail`, `tui/ui/motion`, `tui/streaming_thinking` |
| tui-reducer | L38 | tui | TUI reducers and handlers | 15,064 | `tui/ui/apply`, `tui/ui/handlers`, `tui/ui/dispatch`, `tui/ui/dispatch_prepare`, `tui/ui/session_state`, `tui/ui/compaction_flow`, `tui/ui/approval_routing`, `tui/ui/task_projection`, `tui/ui/provider_routes`, `tui/ui/provider_setup`, `tui/ui/remote_control_bridge` |
| tui-loop | L39 | tui | TUI event loop | 12,682 | `tui/ui/<self>`, `tui/ui/event_loop`, `tui/ui/terminal`, `tui/ui/terminal_input`, `tui/ui/fatal_signal_guard`, `tui/ui/job_control_guard`, `tui/ui/observer_hooks`, `tui/ui/release_check`, `tui/ui/feedback_host` |
| commands-main | L40 | tui | core and navigation commands | 6,089 | `commands/groups/core`, `commands/core_navigation_host`, `commands/core_navigation_voice_host` |
| commands-contract | L41 | tui | command contract dispatch | 8,617 | `commands/contract`, `context_report` |
| cli | L42 | root | composition root: lib.rs and the central tool registry (wiring list) | 16,924 | `<root>`, `tools/registry` |

Sizes: engine is 28,477 lines, the only crate over 25k (decision D4). Every other crate over 15k is bounded below by one indivisible unit: agents (tools/subagent/mod.rs 19.5k), model-client (impl CodewhaleClient spans client, chat, anthropic, responses, system_one), extension-host, runtime-api (runtime_api/<self> 13.7k), thread-host (runtime_threads/manager/<self> 17k), session-store and cli (lib.rs 15k). The three EXISTING crates take only dependency-free units; their current contents are not counted by the checker.

## 2. Layer diagram

Dependencies point down only. A crate may depend on any crate in a lower layer and on none in its own layer or above. Within the TUI band, the bottom crates must never name App.

```
== COMPOSITION ROOT ==
  L42  cli
      |  depends on
      v
== TUI + COMMANDS (may use ratatui/crossterm) ==
  L41  commands-contract
  L40  commands-main
  L39  tui-loop
  L38  tui-frame  tui-reducer
  L37  commands-config  tui-composer
  L36  tui-ambient
  L35  tui-setup
  L34  commands-ext  tui-views-fleet  tui-work
  L33  commands-session  tui-model-pickers  tui-routing
  L32  tui-app
  L31  tui-pickers  tui-views-ext
  L30  tui-views
  L29  tui-state  tui-transcript
  L28  tui-widgets
  L27  tui-chrome
  L26  commands-core  tui-kit
      |  depends on
      v
== HOSTS (no ratatui) ==
  L25  hosts  remote-relay
  L24  runtime-api
  L23  doctor  runtime-api-routes
  L22  thread-host
      |  depends on
      v
== RUNTIME (no ratatui, never names a tui-* or commands-* crate) ==
  L21  engine
  L20  agents-coord  tools-web
  L19  extension-host
  L18  turn-support
  L17  fleet-runtime  tools-ext  tools-workflow
  L16  rlm  session-store  tools-review
  L15  app-server-protocol
  L14  agent-roster
  L13  agents
  L12  authority
  L11  tools-work
  L10  context
  L9   tools-fs
  L8   tools-shell
  L7   hooks-host  tool-runtime
  L6   fleet-roster  provider-local  tool-contract
  L5   model-client
  L4   model-contract  tasks
  L3   billing  route
  L2   mcp-pool  model-catalog  remote-control
  L1   command-contract  telemetry
      |  depends on
      v
  existing workspace crates: core, protocol, config, state, paths, execpolicy, agent-contract, tools, mcp, hooks, ...
```

## 3. The work list (Wave 1)

Every upward edge in the checker JSON, grouped into packets. Wave 1 has a single integrator: one agent lands these on canonical main in small commits, before parallel extraction starts, because nearly every packet edits a hub (tools/spec.rs, core/engine, tui/app, tui/ui/<self>) that a Wave 2 agent would otherwise be moving. Remedy verbs: **move** (relocate an item down to the named crate), **invert** (define a trait or port low and implement it high, wired in cli), **delete**.

Before P0, the source has no checker-visible edges for these, but they block file moves:

- **P0a. `impl App` in commands/contract.rs:4942.** command_contexts_with_config is production code. Move it into tui/app (or make it a free function taking a CommandHost) before commands-contract can exist. This goes with T7.
- **P0b. Avatars reach ratatui.** extension_host/avatars.rs, extension_host/registry.rs and runtime_api/avatars.rs use `codewhale_ratatui::avatar`. Move the avatar-pack data types into a UI-free crate (D6 names it), so extension-host and runtime-api build without the TUI stack.
- **P0c. Glob imports in turn-support.** 6 of its 8 files use `use super::*` over core/engine, which the checker does not follow. Replace them with explicit imports, then rerun the checker. If real coupling appears, those files go back to engine.
- **P0d. A test-support dev crate.** Move crates/tui/src/test_support into it (doc Hosts row). This covers the statement-level `#[cfg(test)]` refs in extension_host/{execution,skills,mod,supervisor}.rs.

### P0d. test-support dev crate and resolver artifacts (2 edges, 17 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| extension-host (L19) → cli (L42) | 16 | extension_host/composition_review→<root>: <root>; extension_host/windows→<root>: <root>; extension_host/command→<root>: <root> | Split. extension_host/{execution,skills,mod,supervisor}.rs hold statement-level #[cfg(test)] crate::test_support uses: move crates/tui/src/test_support into a test-support dev-dependency crate. composition_review and windows are super::supervisor resolver artifacts (no code change; confirm with cargo check). |
| commands-main (L40) → cli (L42) | 1 | commands/groups/core→<root>: test_support | test_support use: covered by the test-support dev crate. |

### P0c. turn-support glob cleanup (1 edges, 1 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| turn-support (L18) → engine (L21) | 1 | core/engine/dispatch→core/engine/<self>: ToolUseState | Move ToolUseState into turn-support. First replace the `use super::*` globs in 6 turn-support files with explicit imports and recount. |

### P1. Approval-decision records out of core/engine (HumanDecision, LivePosture, LiveRuntimeAuthority, preview records) (5 edges, 17 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| mcp-pool (L2) → engine (L21) | 7 | mcp/<self>→core/engine/<self>: HumanDecision | HumanDecision (core/engine/approval.rs:97). mcp only reads &HumanDecision. Founder decision D3 picks the home; the minting constructor from_card_allow must stay unforgeable. |
| tool-contract (L6) → engine (L21) | 5 | tools/spec→core/engine/<self>: HumanDecision, LivePosture, LiveRuntimeAuthority; tools/spec→core/engine/rlm_host: CapturedRlmCaller | LivePosture and LiveRuntimeAuthority move with HumanDecision (D3). CapturedRlmCaller: tools/spec holds it behind the RlmCaller trait from P2. |
| app-server-protocol (L15) → engine (L21) | 3 | tool_inspection→core/engine/preview: active_tool_catalog_sha256; core/session→core/engine/<self>: NextTurnPromptContext; core/ops→core/engine/preview: PreviewRequestInputs | Move active_tool_catalog_sha256 (preview.rs:883, pure hash) into app-server-protocol; move NextTurnPromptContext and PreviewRequestInputs (records) into app-server-protocol. |
| authority (L12) → app-server-protocol (L15) | 1 | core/authority→core/ops: ops | core/authority names core/ops: move the op record it reads into authority, or move authority above app-server-protocol. |
| extension-host (L19) → engine (L21) | 1 | extension_host/mcp→core/engine/<self>: HumanDecision | HumanDecision (D3). |

### P2. RLM caller port (RlmMode down; CapturedRlmCaller stays in engine behind an RlmCaller trait in rlm; NestedCallGate into tool-contract) (6 edges, 18 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| rlm (L16) → engine (L21) | 9 | rlm→core/engine/rlm_host: RlmMode, CapturedRlmCaller, RlmInvocation | Move only RlmMode (pure enum) into rlm. CapturedRlmCaller and its impls (rlm_host.rs, capture(&Engine)) stay in engine behind an RlmCaller trait defined in rlm. RlmInvocation moves only after NestedCallGate is below rlm. |
| tool-contract (L6) → rlm (L16) | 2 | tools/spec→rlm: SessionObjectSnapshot, rlm/session | Move SessionObjectSnapshot into tool-contract (pure record). |
| tool-contract (L6) → tools-ext (L17) | 2 | tools/spec→tools/codemode: NestedCallGate, ExtensionCaller | Move NestedCallGate and ExtensionCaller (records) from tools/codemode into tool-contract. |
| tools-ext (L17) → engine (L21) | 2 | tools/codemode→core/engine/tool_execution: OperationSpanGuard; tools/codemode→core/engine/<self>: Engine | OperationSpanGuard into telemetry or app-server-protocol; codemode reaches Engine through the RlmCaller/NestedCall port. |
| rlm (L16) → tools-ext (L17) | 2 | rlm→tools/codemode: NestedCallGate | NestedCallGate into tool-contract (same move as tool-contract->tools-ext). |
| app-server-protocol (L15) → rlm (L16) | 1 | core/events→rlm: NESTED_RLM_STATUS_PREFIX | Move NESTED_RLM_STATUS_PREFIX into app-server-protocol. |

### P3. ExtensionHostPort: tools, hooks, fleet and mcp reach the extension host through a trait, not ExtensionHostManager (8 edges, 25 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| mcp-pool (L2) → extension-host (L19) | 8 | mcp/<self>→extension_host/native_mcp: epoch, NativeMcpRef, for_plugins; mcp/<self>→extension_host/<self>: validate_caller_plugins; mcp/<self>→extension_host/mcp: SdkTransport | Move the NativeMcpRef record into mcp-pool. Invert epoch/for_plugins/validate_caller_plugins as a NativeMcpSource port defined in mcp-pool, implemented by extension-host and injected by cli. SdkTransport holds Arc<ExtensionHostManager>/HostProcess/Session: it stays in extension-host and is reached only through that port. |
| tools-work (L11) → extension-host (L19) | 4 | tools/speech→extension_host/<self>: StockOperation, manager | manager()/StockOperation: define an ExtensionHostPort trait and the StockOperation DTO in tool-contract; tools reach the host through ToolContext. |
| tools-review (L16) → extension-host (L19) | 4 | tools/review_host→extension_host/<self>: manager, StockOperation; tools/github→extension_host/<self>: manager | Same ExtensionHostPort as tools-work. |
| tools-fs (L9) → extension-host (L19) | 2 | tools/pdf→extension_host/<self>: manager; tools/image_ocr→extension_host/<self>: manager | ExtensionHostPort. |
| tools-shell (L8) → extension-host (L19) | 2 | tools/validate_data→extension_host/<self>: manager, StockOperation | ExtensionHostPort. |
| tools-ext (L17) → extension-host (L19) | 2 | tools/plugin→extension_host/<self>: ExtensionHostManager, manager | ExtensionHostPort; ExtensionHostManager itself is never named by tools. |
| hooks-host (L7) → extension-host (L19) | 2 | hooks→extension_host/<self>: ExtensionHostManager, manager | ExtensionHostPort. |
| fleet-roster (L6) → extension-host (L19) | 1 | fleet/roster→extension_host/<self>: native_presets_for_plugins | native_presets_for_plugins through ExtensionHostPort. |

### P4. Tool executor helpers out of tools/registry into tool-runtime (4 edges, 11 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| engine (L21) → cli (L42) | 6 | core/engine/tool_execution→tools/registry: mcp_result_to_bounded_rich_tool_result, enforce_tool_authority; core/engine/<self>→tools/registry: override_refusal_notice; core/engine/turn_loop→tools/registry: enforce_tool_authority | Move enforce_tool_authority, mcp_result_to_bounded_rich_tool_result and override_refusal_notice out of tools/registry into tool-runtime. |
| agents-coord (L20) → cli (L42) | 3 | tools/subagent/engine→tools/registry: readonly_evidence_tool_name, readonly_evidence_tool; tools/subagent/coord→tools/registry: ToolRegistryBuilder | Move readonly_evidence_tool(_name) into tool-runtime; coord receives a registry-factory closure instead of naming ToolRegistryBuilder. |
| tools-ext (L17) → cli (L42) | 1 | tools/codemode→tools/registry: tools/registry | codemode names tools/registry: take the registry through ToolContext. |
| agents (L13) → cli (L42) | 1 | tools/subagent/<self>→tools/registry: tools/registry | tools/subagent names tools/registry: take the registry through ToolContext. |

### P5. Subagent/child records into the existing agent-contract crate (slice X8) and a ChildEngineFactory port (8 edges, 14 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| agents-coord (L20) → engine (L21) | 5 | tools/subagent/engine→core/engine/<self>: EngineHandle, Engine, EngineConfig | tools/subagent/engine constructs Engine/EngineHandle/EngineConfig: invert through a ChildEngineFactory trait in agent-contract that engine implements and cli injects. Alternative (D5): move tools/subagent/engine into engine (-5 refs, engine +2.4k). |
| tool-contract (L6) → agents (L13) | 2 | tools/spec→tools/subagent/<self>: ChildActivationRequest; tools/activity→tools/subagent/<self>: subagent | Move ChildActivationRequest into agent-contract. |
| agents (L13) → app-server-protocol (L15) | 2 | tools/subagent/<self>→core/events: core/events, REQUEST_NUDGE_RECEIPT_PREFIX | Move REQUEST_NUDGE_RECEIPT_PREFIX into agent-contract. |
| tasks (L4) → agents (L13) | 1 | task_manager→tools/subagent/<self>: new_shared_worker_authority | new_shared_worker_authority: move the worker-authority record into agent-contract. |
| context (L10) → agents (L13) | 1 | runtime_handoff→tools/subagent/<self>: tools/subagent | runtime_handoff names subagent records: take them from agent-contract. |
| tool-contract (L6) → agents-coord (L20) | 1 | tools/spec→tools/subagent/engine: ChildAuthority | Move the ChildAuthority data shape into agent-contract. |
| agents (L13) → engine (L21) | 1 | tools/subagent/<self>→core/engine/<self>: FOREGROUND_CHILD_SETTLE_GRACE | Move FOREGROUND_CHILD_SETTLE_GRACE into agent-contract. |
| fleet-roster (L6) → agents (L13) | 1 | fleet/worker_runtime→tools/subagent/<self>: tools/subagent | Name the needed subagent record from agent-contract. |

### P6. Model-layer contracts (model-contract, model-catalog, crates/config, crates/state) (11 edges, 28 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| model-client (L5) → context (L10) | 7 | client/<self>→compaction: estimate_request_input_tokens, estimate_text_tokens_conservative; client/chat→runtime_handoff: is_agent_topology_checkpoint, is_restored_agent_topology_checkpoint; turn_route_plan→compaction: CompactionConfig | Move estimate_request_input_tokens and estimate_text_tokens_conservative (pure, compaction.rs) into model-contract; move is_agent_topology_checkpoint predicates into crates/state; move CompactionConfig to model-contract or crates/config. |
| model-catalog (L2) → model-client (L5) | 5 | codex_model_cache→client/<self>: CodewhaleClient; provider_lake→client/<self>: CodewhaleClient, AvailableModel; model_inventory→client/system_one: DecisionRouterRoute | Move AvailableModel (client.rs:138, pure) and DecisionRouterRoute into model-contract. Catalog refresh takes a ModelListFetcher trait in model-contract that model-client implements. |
| model-client (L5) → tools-fs (L9) | 3 | client/responses→image_attach: provider_tool_result_image_refs; client/anthropic→image_attach: provider_tool_result_image_refs; client/chat→image_attach: provider_tool_result_image_refs | Move provider_tool_result_image_refs (image_attach) into model-contract. |
| model-catalog (L2) → route (L3) | 2 | model_inventory→route_runtime: resolve_runtime_route_for_identity | resolve_runtime_route_for_identity: model_inventory takes the resolved route as an argument, or move it with model_inventory into route. |
| model-catalog (L2) → session-store (L16) | 2 | model_relevance→session_manager: SessionMetadata, SessionManager | model_relevance reads SessionMetadata: move the SessionMetadata record into crates/state; SessionManager access through a port. |
| model-catalog (L2) → provider-local (L6) | 2 | provider_lake→client/ollama_catalog: load_model, ollama_native_origin | Move client/ollama_catalog parsing (load_model, ollama_native_origin) into model-catalog. |
| context (L10) → app-server-protocol (L15) | 2 | compaction→core/<self>: model_client; compaction→core/turn: add_usage_to | Move add_usage_to into model-contract; compaction receives the model client as an argument. |
| context (L10) → runtime-api (L24) | 2 | profile_constitution→runtime_api/<self>: runtime_account_api_base | Move runtime_account_api_base into crates/config. |
| model-client (L5) → cli (L42) | 1 | client/<self>→<root>: config | client/<self> names crate::config: import from crates/config directly. |
| context (L10) → engine (L21) | 1 | compaction→core/engine/turn_loop: usage_has_reported_data | Move usage_has_reported_data (pure) into model-contract. |
| route (L3) → model-client (L5) | 1 | route_runtime→client/<self>: CodewhaleClient | route_runtime names CodewhaleClient: take a model-contract trait. |

### P7. Session, task and store helpers (crates/state, crates/paths, UsageSink, LedgerSink) (8 edges, 22 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| tasks (L4) → session-store (L16) | 5 | task_manager→runtime_threads/<self>: RuntimeStoreBinding, runtime_threads, prepare_canonical_sessions_root; operate→session_manager: open_private_lock_file | Move RuntimeStoreBinding, prepare_canonical_sessions_root and open_private_lock_file (path/lock helpers) into existing crates/state (or crates/paths). |
| session-store (L16) → engine (L21) | 3 | runtime_threads/<self>→core/engine/handle: SteerOutcome; runtime_threads/<self>→core/engine/<self>: core/engine; receipts→core/engine/<self>: PERMISSION_POSTURE_LINE | Move SteerOutcome and PERMISSION_POSTURE_LINE into app-server-protocol. |
| context (L10) → session-store (L16) | 3 | compaction→usage_persistence: report_effective_route_for_interactive_origin, report_unreceipted_for_interactive_origin; compaction→session_manager: strip_thinking_tags | Move report_effective_route_for_interactive_origin / report_unreceipted_for_interactive_origin behind a UsageSink trait in context; move strip_thinking_tags (pure) into context. |
| tools-work (L11) → session-store (L16) | 3 | tools/session→session_manager: session_manager, truncate_id, CheckpointSource | tools/session needs SessionManager: reach it through a SessionPort in tool-contract carried by ToolContext; move truncate_id and CheckpointSource into app-server-protocol. |
| agents (L13) → session-store (L16) | 3 | tools/subagent/<self>→todo_snapshot: TodoSource, fork_state_todo_section; tools/subagent/<self>→usage_persistence: report_effective_route_for_interactive_origin | Move TodoSource and fork_state_todo_section (todo_snapshot) into tasks; usage reporting through the UsageSink from context->session-store. |
| authority (L12) → session-store (L16) | 2 | approval_log→session_peek: redact; approval_log→session_manager: default_sessions_dir | Move redact (session_peek) into crates/sanitize and default_sessions_dir into crates/paths. |
| tasks (L4) → fleet-runtime (L17) | 2 | work_graph→fleet/ledger: FleetLedger, fleet/ledger | work_graph reads FleetLedger: define a LedgerSink trait in tasks implemented by fleet-runtime. |
| session-store (L16) → thread-host (L22) | 1 | receipts→runtime_threads/manager/<self>: RuntimeThreadManagerConfig | receipts names RuntimeThreadManagerConfig: move that config record into session-store. |

### P8. Lateral tool-feature helpers into tool-contract (9 edges, 11 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| tool-contract (L6) → tools-shell (L8) | 2 | tools/spec→tools/shell: tools/shell; tools/execution_envelope→tools/shell: agent_readonly_bash_input | Move agent_readonly_bash_input into tool-contract. |
| tasks (L4) → tool-contract (L6) | 2 | tools/plan→tools/spec: tools/spec; tools/todo→tools/spec: tools/spec | tools/plan and tools/todo name tools/spec: move these two tool adapters to tools-work and keep only their state in tasks. |
| tool-contract (L6) → tool-runtime (L7) | 1 | tools/spec→tools/handle: tools/handle | tools/spec names tools/handle: move the handle type the spec references into tool-contract. |
| tool-contract (L6) → hooks-host (L7) | 1 | tools/spec→hooks: HookExecutor | Define the HookExecutor trait in tool-contract; hooks-host implements it. |
| tools-shell (L8) → tools-web (L20) | 1 | tools/shell→tools/web: network_authorized | Move network_authorized into tool-contract. |
| tools-shell (L8) → tools-review (L16) | 1 | tools/run_tool→tools/verifier: verifier | run_tool names tools/verifier: invert as a Verifier trait in tool-contract. |
| tools-ext (L17) → tools-web (L20) | 1 | tools/rlm→tools/fetch_url: FetchUrlTool | tools/rlm names FetchUrlTool: reach it through the registry, not by type. |
| tools-shell (L8) → tools-fs (L9) | 1 | tools/validate_data→tools/file: resolve_guarded_read_path | Move resolve_guarded_read_path into tool-contract. |
| tools-review (L16) → engine (L21) | 1 | tools/review_core→core/engine/streaming: contains_fake_tool_wrapper | Move contains_fake_tool_wrapper (pure, core/engine/streaming) into tool-contract. |

### P9. Hosts and doctor helpers (3 edges, 9 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| doctor (L23) → cli (L42) | 4 | doctor_fix→<root>: McpServerDoctorStatus, doctor_check_mcp_server, default_tools_dir | Move McpServerDoctorStatus, doctor_check_mcp_server and default_tools_dir out of lib.rs into doctor. |
| runtime-api-routes (L23) → runtime-api (L24) | 4 | runtime_api/thread_history→runtime_api/<self>: map_thread_err; runtime_api/secrets→runtime_api/<self>: runtime_account_api_base | Move map_thread_err and runtime_account_api_base down into runtime-api-routes (or crates/config for the URL helper). |
| runtime-api (L24) → hosts (L25) | 1 | runtime_api/<self>→acp_server: capture_frontend | capture_frontend: move into runtime-api or invert as a callback registered by acp_server. |

### T1. Underwater enums (ShellPhase, LiveActivityKind, ...) into tui-state (6 edges, 24 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| tui-chrome (L27) → tui-ambient (L36) | 7 | tui/ambient_life→tui/underwater: LiveActivityKind; tui/ocean→tui/underwater: ShellPhase | Move LiveActivityKind and ShellPhase (pure Copy enums, tui/underwater.rs) into tui-state. |
| tui-app (L32) → tui-ambient (L36) | 6 | tui/app/<self>→tui/underwater: LaunchAction, permission_label; tui/app/status→tui/underwater: ShellPhase; tui/app/<self>→tui/pet_watch: PetWatch | Move LaunchAction, permission_label and ShellPhase into tui-state; hold PetWatch as Option<Box<dyn AmbientCompanion>> (trait in tui-state) implemented by tui-ambient. |
| tui-views (L30) → tui-ambient (L36) | 4 | tui/views/<self>→tui/underwater: mode_label, header_permission_ink, header_mode_ink; tui/whales→tui/underwater: ShellPhase | mode_label, header_permission_ink, header_mode_ink and ShellPhase into tui-state. |
| tui-widgets (L28) → tui-ambient (L36) | 4 | tui/widgets→tui/underwater: ShellPhase, LiveActivity, decorative_shell_motion_enabled | ShellPhase, LiveActivity and decorative_shell_motion_enabled into tui-state. |
| tui-work (L34) → tui-ambient (L36) | 2 | tui/phase_strip→tui/pet_watch: main_view; tui/phase_strip→tui/underwater: posture_chips | posture_chips into tui-state; phase_strip takes pet main_view through the AmbientCompanion trait. |
| tui-routing (L33) → tui-ambient (L36) | 1 | tui/herdr→tui/underwater: ShellPhase | ShellPhase into tui-state (same move). |

### T2. App state records into tui-state (6 edges, 21 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| tui-app (L32) → tui-work (L34) | 9 | tui/app/init→tui/work_surface: WorkSurfacePlacement, WorkSurfaceState, RailPanel; tui/app/<self>→tui/work_surface: WorkSurfaceState, RailPanel; tui/app/<self>→tui/hotbar: HotbarActionRegistry | Move the pure enums WorkSurfacePlacement and RailPanel, plus HotbarActionRegistry, InfoLineHitbox and FinishedWork state records, down into tui-state. WorkSurfaceState holds TerminalDock and SettledFileActivity and pub(super) fields: it stays in tui-work behind an accessor/port that App holds as Box<dyn>. |
| tui-widgets (L28) → tui-work (L34) | 5 | tui/widgets→tui/infoline: source_theme; tui/widgets→tui/agent_focus: composer_chip_text, composer_placeholder; tui/widgets→tui/phase_strip: context_percent_from_app | Move source_theme, composer_chip_text, composer_placeholder and context_percent_from_app (pure helpers) into tui-widgets. |
| tui-app (L32) → tui-composer (L37) | 2 | tui/app/<self>→tui/prompt_suggestion: SuggestionRouteAuthority; tui/app/<self>→tui/mouse_ui: ComposerClickTrace | Move SuggestionRouteAuthority and ComposerClickTrace (records) into tui-state. |
| tui-app (L32) → tui-model-pickers (L33) | 2 | tui/app/<self>→tui/model_picker: picker_efforts_for_route, effective_tier_for_route | Move picker_efforts_for_route and effective_tier_for_route (pure) into tui-state or route. |
| tui-app (L32) → tui-routing (L33) | 2 | tui/app/<self>→tui/tool_routing: release_oldest_tool_detail_outputs; tui/app/init→tui/notifications: settings | release_oldest_tool_detail_outputs becomes a callback; notification settings record into tui-state. |
| tui-chrome (L27) → tui-work (L34) | 1 | tui/tideline→tui/work_surface: RailPanel | RailPanel into tui-state (same move as tui-app -> tui-work). |

### T3. ViewContext trait: views, widgets and chrome stop taking &App (7 edges, 32 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| tui-views (L30) → tui-app (L32) | 7 | tui/views/<self>→tui/app/<self>: App, SidebarRowAction; tui/views/workflows_manager→tui/app/<self>: App; tui/views/mode_picker→tui/app/<self>: AppModeUi | Views take a narrow ViewContext trait (in tui-state) that App implements; move SidebarRowAction and AppModeUi into tui-state. |
| tui-chrome (L27) → tui-app (L32) | 6 | tui/footer_ui→tui/app/<self>: App; tui/vim_mode→tui/app/<self>: tui/app; tui/tideline→tui/app/<self>: App | Pass the narrow state footer_ui, vim_mode and tideline need (density, mode flags) instead of &App; ComposerDensity to tui-state. |
| tui-state (L29) → tui-app (L32) | 5 | tui/session_metrics→tui/app/<self>: App; tui/session_boot→tui/app/<self>: App; tui/workspace_context→tui/app/<self>: App | session_metrics, session_boot and workspace_context take the fields they read, not &App (ViewContext from T3). |
| tui-widgets (L28) → tui-app (L32) | 5 | tui/widgets→tui/app/<self>: tui/app, bound_agent_activity_text, app | Move bound_agent_activity_text (pure) into tui-widgets; widgets read ViewContext, not App. |
| tui-transcript (L29) → tui-app (L32) | 4 | tui/redaction_gate→tui/app/<self>: tui/app; tui/transcript→tui/app/<self>: TranscriptSpacing; tui/history→tui/app/<self>: TranscriptSpacing | Move TranscriptSpacing into tui-state; redaction_gate reads ViewContext. |
| tui-app (L32) → tui-setup (L35) | 3 | tui/app/init→tui/onboarding: is_onboarded, locale_confidently_inferred; tui/app/<self>→tui/onboarding: mark_onboarded | Move is_onboarded, locale_confidently_inferred and mark_onboarded (onboarding state) into tui-state. |
| tui-views-ext (L31) → tui-app (L32) | 2 | tui/views/extensions→tui/app/<self>: App; tui/views/automations→tui/app/<self>: App | ViewContext. |

### T4. Terminal and loop helpers out of tui/ui/<self> (7 edges, 16 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| tui-composer (L37) → tui-loop (L39) | 8 | tui/external_editor→tui/ui/<self>: ui, pause_terminal_input_for_child; tui/mouse_ui→tui/ui/<self>: tui/ui | Move tui/ui/terminal_input (pause_terminal_input_for_child and its static CHILD_TERMINAL_GATE) whole into tui-state; no port needed. |
| tui-work (L34) → tui-loop (L39) | 2 | tui/phase_strip→tui/ui/<self>: context_usage_snapshot; tui/work_surface→tui/ui/<self>: context_usage_snapshot | Move context_usage_snapshot (tui/ui/<self>) into tui-state. |
| tui-reducer (L38) → tui-loop (L39) | 2 | tui/ui/apply→tui/ui/observer_hooks: observer_hooks; tui/ui/apply→tui/ui/feedback_host: feedback_host | apply names observer_hooks/feedback_host: move both small units into tui-reducer. |
| tui-chrome (L27) → tui-loop (L39) | 1 | tui/tideline→tui/ui/<self>: context_usage_snapshot | context_usage_snapshot into tui-state (same move). |
| tui-routing (L33) → tui-loop (L39) | 1 | tui/control_socket→tui/ui/<self>: tui/ui | control_socket names tui/ui: route through an event sender, not the loop module. |
| tui-state (L29) → tui-loop (L39) | 1 | tui/pending_requests→tui/ui/<self>: push_approval_request_view | push_approval_request_view: pending_requests returns the request; the loop pushes the view. |
| tui-app (L32) → tui-loop (L39) | 1 | tui/app/<self>→tui/ui/feedback_host: EditReady | Move EditReady (record) into tui-state. |

### T5. Small pure TUI helpers (10 edges, 25 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| tui-ambient (L36) → tui-composer (L37) | 5 | tui/underwater→tui/file_mention: visible_mention_menu_entries, apply_mention_menu_selection; tui/underwater→tui/slash_menu: visible_slash_menu_entries, apply_slash_menu_selection; tui/underwater→tui/composer_ui: composer_submit_chord | Move visible_mention_menu_entries/apply_mention_menu_selection, visible_slash_menu_entries/apply_slash_menu_selection and composer_submit_chord out of tui/underwater callers: underwater should call them through the composer, so move those render call sites into tui-composer or pass menu entries as data. |
| tui-widgets (L28) → tui-transcript (L29) | 5 | tui/widgets→tui/history: tui/history, detect_tool_runs_from_slices, tool_run_summary; tui/widgets→tui/transcript: StreamingSourceReceipt; tui/widgets→tui/active_cell: ActiveCell | Move detect_tool_runs_from_slices, tool_run_summary, StreamingSourceReceipt and ActiveCell's read shape into tui-widgets or tui-state. |
| tui-chrome (L27) → tui-transcript (L29) | 3 | tui/footer_ui→tui/history: summarize_tool_output; tui/selection→tui/history: TranscriptActionOwner; tui/output_rows_cache→tui/history: OutputRow | Move summarize_tool_output, TranscriptActionOwner and OutputRow into tui-chrome. |
| tui-work (L34) → tui-composer (L37) | 3 | tui/agent_details→tui/mouse_ui: agent_transcript_evidence_available; tui/hotbar→tui/command_palette: tui/command_palette; tui/work_surface→tui/mouse_ui: clear_transcript_selection | Move agent_transcript_evidence_available and clear_transcript_selection into tui-state; hotbar takes command palette entries as data. |
| tui-chrome (L27) → tui-widgets (L28) | 2 | tui/shell_key_routing→tui/widgets: is_altgr | Move is_altgr (pure key predicate) into tui-chrome. |
| tui-setup (L35) → tui-composer (L37) | 2 | tui/launch_resume_confirm→tui/mouse_ui: mouse_hits_rect | Move mouse_hits_rect (pure geometry) into tui-kit. |
| tui-pickers (L31) → tui-composer (L37) | 2 | tui/file_picker→tui/file_picker_relevance: modified_workspace_paths | Move modified_workspace_paths into tui-state. |
| tui-transcript (L29) → tui-setup (L35) | 1 | tui/redaction_gate→tui/onboarding: wrap_words | Move wrap_words (pure) into tui-kit. |
| tui-chrome (L27) → tui-routing (L33) | 1 | tui/footer_ui→tui/subagent_routing: tui/subagent_routing | footer_ui names subagent_routing: pass the summary as data. |
| tui-chrome (L27) → tui-composer (L37) | 1 | tui/key_actions→tui/file_mention: file_mention_body | Move file_mention_body (pure) into tui-chrome. |

### T6. View contract (ViewStack, ModalKind, CommandPaletteAction, shared records) into tui-state (11 edges, 20 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| tui-transcript (L29) → tui-views (L30) | 4 | tui/pager→tui/views/<self>: tui/views, CommandPaletteAction; tui/redaction_gate→tui/views/<self>: tui/views; tui/live_transcript→tui/views/<self>: tui/views | Move CommandPaletteAction and the ViewStack/ModalKind view contract out of tui/views/<self> into tui-state. |
| tui-views (L30) → tui-views-fleet (L34) | 2 | tui/views/<self>→tui/views/fleet_detail: FleetRouteTarget | Move FleetRouteTarget into tui-state. |
| tui-views (L30) → tui-setup (L35) | 2 | tui/views/<self>→tui/setup: GuidedConstitutionDraft, SetupRuntimePreset | Move GuidedConstitutionDraft and SetupRuntimePreset into tui-state. |
| tui-views (L30) → tui-composer (L37) | 2 | tui/views/<self>→tui/context_menu: ContextMenuEntry; tui/views/help→tui/composer_ui: terminal_can_report_shift_enter | Move ContextMenuEntry and terminal_can_report_shift_enter into tui-state / tui-kit. |
| tui-views (L30) → tui-model-pickers (L33) | 2 | tui/views/<self>→tui/provider_picker: ProviderPickerView; tui/views/<self>→tui/model_picker: picker_efforts_for_route | ViewStack holds ProviderPickerView as Box<dyn View>; picker_efforts_for_route into tui-state. |
| tui-views (L30) → tui-work (L34) | 2 | tui/views/<self>→tui/phase_strip: TidelineFooter, render_tideline_footer | Move TidelineFooter/render_tideline_footer into tui-chrome (tideline already lives there). |
| tui-widgets (L28) → tui-views (L30) | 2 | tui/approval→tui/views/<self>: tui/views | Approval card names tui/views: depend on the view contract moved to tui-state. |
| tui-chrome (L27) → tui-views (L30) | 1 | tui/shell_key_routing→tui/views/<self>: ModalKind | ModalKind into tui-state (view contract). |
| tui-model-pickers (L33) → tui-views-fleet (L34) | 1 | tui/model_picker→tui/views/fleet_detail: tui/views/fleet_detail | FleetRouteTarget into tui-state (same move). |
| tui-views-ext (L31) → tui-composer (L37) | 1 | tui/views/extensions→tui/context_menu: ContextMenuEntry | ContextMenuEntry into tui-state (same move). |
| tui-views-ext (L31) → tui-views-fleet (L34) | 1 | tui/views/automations→tui/views/fleet_setup: fleet_setup | automations names fleet_setup: open it through ViewStack by kind. |

### T7. CommandHost trait and command-group registration (6 edges, 13 refs)

| edge | refs | top items (src→dst: items) | remedy |
|---|---:|---|---|
| commands-config (L37) → commands-contract (L41) | 4 | commands/config_host→commands/contract: resolve, apply_named_replacements; commands/config_main_import_host→commands/contract: run_async; commands/debug_group→commands/contract: contract | Move resolve, apply_named_replacements and run_async out of commands/contract into commands-core. |
| commands-core (L26) → tui-app (L32) | 4 | commands/<self>→tui/app/<self>: App, tui/app; commands/user_registry→tui/app/<self>: tui/app; commands/traits→tui/app/<self>: App | Command traits take a CommandHost trait (in commands-core) that App implements, instead of &mut App. Same move as the impl App fix in P0a. |
| commands-core (L26) → commands-config (L37) | 2 | commands/groups/<self>→commands/config_group: config_group; commands/groups/<self>→commands/debug_group: debug_group | commands/groups/<self> lists config_group/debug_group: move the group list into cli (registration) or commands-contract. |
| commands-session (L33) → commands-contract (L41) | 1 | commands/session_group→commands/contract: contract | Same as commands-core -> commands-contract. |
| commands-core (L26) → commands-contract (L41) | 1 | commands/traits→commands/contract: contract | commands/traits names contract: move the contract types it needs into commands-core. |
| commands-core (L26) → commands-session (L33) | 1 | commands/groups/<self>→commands/session_group: session_group | Same group-list move. |

Totals: runtime and host packets P0d–P9 carry 173 refs; TUI packets T1–T7 carry 151 refs. Together: 324, which equals the checker's upward_refs.

### Which Wave 1 packets are independent

Two packets are independent when they edit disjoint source files. Independent packets can land in any order, and in separate commits that do not conflict.

- **Independent of everything else:** P0b (avatars), P0c (turn-support globs, core/engine/{dispatch,reviewer,...} only), P0d (test_support), P6 (client/*, provider_lake, model_inventory, compaction's estimators), P9 (runtime_api, doctor, lib.rs doctor helpers).
- **One chain through tools/spec.rs and ToolContext:** P1 → P2 → P3 → P4 → P8. Each one edits tools/spec.rs, so they land one after another, in this order. P1 and P2 also both edit core/engine.
- **P5** (tools/subagent/<self>, tools/subagent/engine, agent-contract) is independent of P6, P7 and P9. It touches tools/spec.rs only for ChildActivationRequest and ChildAuthority, so land it after P4.
- **P7** (session_manager, task_manager, work_graph, approval_log) is independent of P1–P5. It shares compaction.rs with P6, so land P6 first or batch the two together.
- **The TUI packets touch no runtime file**, so they are independent of every P packet. Among themselves: T1, T4 and T5 each add new records to tui-state and edit disjoint files, so they are mutually independent. T2, T3 and T6 all edit tui/app and tui/views/<self>, so serialize them: T2 → T6 → T3. T7 goes with P0a (commands/contract, tui/app) and comes after T3.
- **The biggest rebuild lever** (parallelism judge) is P5's move of SubAgentStatus, AgentRunUsage, AgentWorkerStatus, AgentWorkerSpec, UserFollowUpOutcome, MailboxMessage and FleetMemberStart out of tools/subagent/<self> into agent-contract. In this map those refs point down, not up, but tools/subagent/<self> (396 commits since 2026-08-01) currently rebuilds most of the workspace. The same lever applies to ShellStatus and ShellCompletionEvent (tools/shell.rs) moving into tool-contract.

## 4. Wave 2: parallel extraction

A crate can be extracted from codewhale-tui once every crate it depends on is already out, so extraction runs bottom-up. Once Wave 1 is finished, each round below is an antichain: no crate in a round depends on another crate in the same round. Each round's crates can go to separate agents at the same time. A crate is ready as soon as its own dependencies are out, so a lagging chain does not block an unrelated one.

| round | crates (one agent each) |
|---:|---|
| 1 | command-contract, mcp-pool, model-catalog, remote-control, telemetry, tui-kit |
| 2 | billing, route |
| 3 | model-contract, tasks |
| 4 | model-client |
| 5 | fleet-roster, provider-local, tool-contract |
| 6 | hooks-host, tool-runtime |
| 7 | tools-shell |
| 8 | tools-fs |
| 9 | context |
| 10 | tools-work |
| 11 | authority, tui-chrome |
| 12 | agents |
| 13 | agent-roster |
| 14 | app-server-protocol |
| 15 | rlm, session-store, tools-review |
| 16 | fleet-runtime, tools-ext, tools-workflow |
| 17 | turn-support |
| 18 | extension-host |
| 19 | agents-coord, commands-core, tools-web |
| 20 | engine, tui-widgets |
| 21 | thread-host, tui-state, tui-transcript |
| 22 | doctor, runtime-api-routes, tui-views |
| 23 | runtime-api, tui-pickers, tui-views-ext |
| 24 | hosts, remote-relay, tui-app |
| 25 | commands-session, tui-model-pickers, tui-routing |
| 26 | commands-ext, tui-views-fleet, tui-work |
| 27 | tui-setup |
| 28 | tui-ambient |
| 29 | commands-config, tui-composer |
| 30 | tui-frame, tui-reducer |
| 31 | tui-loop |
| 32 | commands-main |
| 33 | commands-contract |
| 34 | cli |

What this means in practice:

- Rounds 1–6 are the hubs: model-catalog, billing, route, model-contract, model-client, tasks, tool-contract and tool-runtime have 50–60 transitive dependents each. Extract each with a single owner and freeze its API before round 7 starts.
- The runtime band is a long chain: tools-shell → tools-fs → context → tools-work → authority → agents → app-server-protocol → session-store → … → engine → thread-host. This is because feature crates sit below engine (D1). In the runtime band, the work runs mostly one crate at a time. If the founder picks the doc order under D1, the tool features become parallel leaves, at a cost of more Wave 1 refs.
- The TUI band can start early. tui-kit is ready in round 1. tui-chrome is ready in round 11 and tui-widgets in round 20, alongside the runtime chain. The TUI tail fans out three ways at rounds 21–26: tui-state/tui-transcript; tui-views/tui-pickers/tui-views-ext; then commands-session, tui-model-pickers, tui-routing, commands-ext, tui-views-fleet and tui-work.
- Hosts (thread-host, runtime-api-routes, doctor, runtime-api, hosts, remote-relay) follow engine in rounds 21–25 and can run in parallel with the TUI views.
- cli (lib.rs plus tools/registry) is what remains of codewhale-tui at the end.
- Serialize shared builds: at most two native builds per machine (codewhale/scripts/build-lock.py). Each agent proves its extraction with `cargo check -p <crate>` plus the existing guard tests (single_turn_loop and others) for the crate it moved.

## 5. Decisions for the founder

- **D1. Where feature crates sit relative to engine (contested).** The architecture judge ranked doc-faithful first, because the doc puts features as leaves above the runtime. The executability and parallelism judges ranked min-cut and agent-partition first. This map keeps the tool features, agents, extension-host and fleet below engine, since the engine calls them directly. Measured cost of the doc order on this exact map: all features and agents above engine gives 505 refs (+181). A partial version (tools-web, tools-review, tools-workflow, tools-work, agents-coord and fleet-runtime above engine) gives 370 refs (+46). The doc order is reached later, through the contributor-trait inversion (doc rule 3), not through a different map.
- **D2. Where tools/registry lives.** It is in cli (the composition root's wiring list), not in its own tool-catalog crate. That costs +4 refs, and P4 moves 11 refs of executor helpers out of it. The alternative is agent-partition's tool-catalog crate below engine.
- **D3. HumanDecision's home and its trust boundary.** from_card_allow is pub(super) by design (the comment reads 'Only the engine's card resolver can build one'). mcp-pool sits at L2, so any home it can name must be low: the existing execpolicy crate or protocol, below codewhale-tui. Rust cannot seal a constructor across crates. The options: (a) move the struct and the card-allow minting function together into execpolicy, so minting stays private to one crate; (b) keep HumanDecision in engine and pass consumers a read-only view (a weaker guarantee); (c) accept a pub constructor guarded by a grep or guard test. Recommendation: (a).
- **D4. engine at 28,477 lines, over the 25k line.** 18 files under core/engine carry inherent `impl Engine` blocks, which cannot cross a crate boundary. The turn-support peel (4,529 lines) holds only if P0c finds no hidden glob coupling.
- **D5. Lumping trades left out of the map.** Moving tools/subagent/engine (the child-engine adapter) into engine saves 5 refs and makes engine 30.9k; this map keeps it in agents-coord behind a ChildEngineFactory port. Moving rlm into engine saves 8 refs and makes engine 29.9k; this map keeps rlm separate behind the RlmCaller port.
- **D6. Naming against existing workspace crates.** This map's `cli` means the remainder of crates/tui (codewhale-tui, lib.rs plus registry), not the existing crates/cli. Rename it, for example to tui-root, or keep it as codewhale-tui. New names overlap existing crates: mcp-pool and crates/mcp, hooks-host and crates/hooks, tool-contract/tool-runtime and crates/tools, authority and crates/execpolicy, commands-* and crates/commands. For each pair, choose merge or distinct name, and measure the existing crate's size before merging. P0b needs a home for the avatar-pack types (crates/plugins or a new avatar-contract).
- **D7. session-store at 18.3k with export, peek, resume, reconcile, purge and receipts merged in.** The architecture judge wants one store-mutation authority; agent-partition had split session-ops out. Merged here.
- **D8. Doc amendments.** TUI_DECONSTRUCTION.md's Target map table conflicts with this map. Differences: provider crates stay in model-client (impl CodewhaleClient); hooks and mcp become new crates; runtime_api is not absorbed into app-server; integrations is in tui-setup; the commands-* split. The doc also says the TUI depends only on app-server-protocol. This map does not enforce that: the TUI crates still name runtime crates through downward edges, and cutting those is a later inversion phase. Amend the doc in the same slice that adopts this map. canonical docs/design/TUI_DECONSTRUCTION.md is currently modified in the working tree by another writer.

### Checker limits, as the checker states them

> Paths are resolved textually. Second-hop names (`use super::spec;` then `spec::ToolSpec`), glob re-exports, macros and `#[path]` includes are not followed, and an inline non-test `mod` resolves `super::` one level too high. … Treat the output as a work list and a ratchet; `cargo check` stays the authority.

What follows from that:

- Real coupling is higher than 324. Grouped imports (`use super::{A,B}`) and globs (the turn-support files, runtime_api/*) go uncounted, so every packet must be confirmed with `cargo check`.
- The checker sees `#[cfg(test)]` statement-level uses as production refs (extension-host → cli). The `super::` limit creates resolver artifacts (composition_review, windows).
- It does not check inherent-impl placement. The final map was hand-checked against the agent-partition impl scan plus the judges' findings, and P0a is the one known violation. It does not check orphan-rule breakage for trait impls between two moved crates either.
- It does not see external-crate usage such as codewhale_ratatui (P0b), and it cannot enforce the 'TUI only via app-server-protocol' rule.
- No cargo, build or test command was run for this map. All numbers come from the checker and the local evaluator on the live graph.

## 6. Independent review (2026-10-31)

A read-only review (Codex CLI, `gpt-6.1-sol`) reproduced the checker line and
found that wave 1 as written is not enough. Fix these before scheduling wave 2:

1. `core/protocol_parity.rs:1306,1314` implement `Event` and `Op` inherently;
   the file goes to thread-host and the types to app-server-protocol (E0116).
   Use the existing `event_to_protocol`/`op_to_protocol` free functions.
2. `tools/registry` defines `ToolRegistry` and `ToolRegistryBuilder`, which the
   engine builds (`core/engine.rs:5939`) via the `tools/mod.rs:100` re-export.
   Put the registry machinery below the engine and inject the concrete list.
3. `runtime_api/review` is an Axum handler on `RuntimeApiState`, not a DTO;
   `runtime-api-routes` reaches private `RuntimeApiState` fields
   (`runtime_api.rs:192`).
4. `tui/ui/overlays.rs:220` ↔ `tui/ui/handlers.rs:923` couple tui-frame and
   tui-reducer through `use super::*`.
5. Several packet remedies put shared contracts above a consumer (P6
   `DecisionRouterRoute`, P2 vs P1 `RlmCaller`, T1/T6 into tui-state at L29).
   Recompute layers after wave 1.
6. `tools/spec.rs:24,120,591,773` holds concrete `SharedRlmSessionStore` and
   `SharedShellManager`; inject them.
7. D3(a) breaks the minting call at `core/engine/turn_loop.rs:3382`; design
   the cross-crate approval authority explicitly.
8. `ViewStack` (`tui/views/mod.rs:1263`) already uses trait objects but
   downcasts to concrete views (lines 1602, 1612, 1635).
9. `CommandResult` is `CommandResult<AppAction>` (`commands/mod.rs:118`).
10. `pub(crate)`/`pub(super)` items and a `#[path]` mount
    (`commands/contract.rs:307`) cross proposed boundaries.
11. `cached_command_list!` (`commands/groups/mod.rs:8`) is lexical.
12. Embedded assets: extension bundles, browser assets, changelog, the
    pet-watch TSV.
13. `build.rs` env vars (`CODEWHALE_BUILD_VERSION`), `option_env!`, and
    `cfg(test)` mocks do not follow moved files.
14. P0c is wrong: the turn-support globs are in test modules.

Because most of these sit inside a band, the first step is the coarse split in
[COARSE_SPLIT.md](COARSE_SPLIT.md).
