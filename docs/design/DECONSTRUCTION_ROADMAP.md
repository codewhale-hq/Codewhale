# Deconstruction roadmap: 0.10.2 to 0.11.0

Status: plan, produced 2026-10-11 from a read-only analysis of eight candidate
carve-outs, a sequencer and an adversarial critic (this is the critic's
corrected version). It extends `docs/design/TUI_DECONSTRUCTION.md`; where the
two differ, this file is the release schedule and that file is the design.
Numbers marked "proxy" were measured from git-tracked files, not from a real
`cargo package`; re-measure at each release gate. Nothing here is a commitment
to a date: at the observed cadence (0.10.0 on 2026-09-22, 0.10.1 on
2026-10-07) ten patch releases is months of calendar time.

## Principles
- Size guard verdict (answers the user's question): keep the 10 MiB hard fail, it is not what makes life harder. check-crate-sizes.py is called only from scripts/release/publish-crates.sh, after the all-crate `cargo publish --dry-run` has already built every tarball, so its marginal cost is about zero. It exists because 0.10.1 uploaded 26 crates and tui was refused with 413 at 11.93 MiB. What does make life harder is (a) the 94.7% headroom, and (b) the sequencer's per-slice size budgets (9.44, 9.43, 9.42, 9.38 ...), which are below measurement noise and force a full 29-crate dry-run per slice. Drop the per-slice budgets. The only size gate is 'no crate grows' at the release cut.
- Measured today (read-only, tar.gz of the git-tracked files in crates/tui that survive the Cargo.toml exclude list, so a proxy for `cargo package`): v0.9.13 6.95 MiB, v0.10.0 7.34, v0.10.1 9.48, HEAD 9.50. The reported real tarball is 9.47, so the proxy is within about 0.03 MiB. Growth is bursty (+2.1 MiB in one cycle), so 0.5 MiB of headroom can vanish in a single release. That is the case for keeping the hard fail. Compressed cost is about 8.9 bytes per source line (src is 8.33 MiB over 963k non-excluded lines), so a 3k-line move frees about 0.03 MiB. Leaf carve-outs cannot clear the 9.0 MiB warn line. Only a UI/runtime split does: proxy UI 2.78 MiB, runtime remainder 6.89 MiB (of which extension-host, plugins and assets are 1.16 MiB).
- Size is evidence for the 0.11 split, not for any leaf. The sequencer said 'do not use size as justification'. That is wrong at the top: the only move that changes tui's headroom is the atomic one, so size is a legitimate input to the A/B founder decision. It is never a reason for a particular leaf carve-out.
- The A/B decision comes first, because it decides which carve-outs are on the critical path. If the SCC is moved atomically (Option A), X1, X4, X7, X8, P1, P2 and settings are not prerequisites. They only shrink a cycle that moves as one unit anyway. What A needs is: prod edges 0 (X2), test edges 0, late 0, uilib 0, and a lower test-support. If the founder picks the design doc's slice-only path (Option B), those carve-outs are the work. The sequencer put 9 carve-out releases ahead of the decision. This plan puts the shared prefix first and the decision at 0.10.5.
- One structural move per patch release. A move is a rename-dominated relocation or one seam-introducing slice with a named gate. Changes that do not alter the crate graph ride along: test-only edge cuts, packaging, docs, the guard, ratchet baseline updates. A release that carries both a seam and its move is two moves. P2 and X8 are split accordingly.
- Two gate tiers. Slice gate (per landing on main): batched edits, one compile, `module_graph.py --check`, the existing tests for the touched filters with real 'N passed' (N > 0), clippy with CI's flags. Release gate (per patch release, once): `scripts/release/publish-crates.sh dry-run` (multi-package `cargo publish --dry-run`, which includes check-crate-sizes.py), validate-crate-publish-order.py, and green hosted CI (macOS and Windows) on the exact SHA. Per-slice `cargo package` of a single crate does not work for unpublished 0.10.x workspace dependencies. Only the multi-package dry-run resolves them, and it builds every tarball, so it runs at the release cut, not per slice.
- Reuse before add. Existing homes first: execpolicy, codewhale-secrets, codewhale-models (it owns codewhale_models::Tool, the provider request types and the client contract), codewhale-config, codewhale-runtime. X4 therefore lands in codewhale-models, not in crates/tools (which depends only on protocol and publishes before config and models) and not in a new util-schema package. A new package name is a founder gate (test-support is publish=false and exempt).
- Every move leaves a `pub use` shim at the old path (3,017 crate::config refs, 330 test_support importers, 165 session_manager refs). The shim is deleted in a later cleanup, never in the move. A glob `pub(crate) use` shim for a test-only crate is fine.
- The ratchet only goes down, and the baseline in scripts/runtime-boundary-baseline.json (prod 11, test 35, late 1, uilib 1, verified today as PASS over 117 modules) is updated in the same commit that moves the code. The sequencer's ratchet_targets kept test 35, late 1, uilib 1 through every release and then demanded 'test <= 34' at 0.11.0. Nothing in the old plan lowered test edges (tools|tui 12, test_support|tui 7, client|tui 3, session_manager|tui 3, fleet|lib.rs 3, config|tui 2, core|tui 2, runtime_api|commands 2, prompts|tui 1). The late edge (core/engine/tests/test_cases_04.rs:1197 uses crate::exec_agent) and the uilib edge (tools/subagent/tests/roster_routes.rs:798 uses crossterm) are tests. Each has to reach 0 before any new crate that must never depend on tui or crossterm (crates/runtime/Cargo.toml says so) can hold that code.
- Test edges and test code are not in the tarball: tests/**, **/tests.rs, **/tests/**, *_tests.rs and test_cases_*.rs are excluded by crates/tui/Cargo.toml. Moving tests therefore frees 0 bytes of tarball. Inline `#[cfg(test)]` blocks in non-excluded files (schema_sanitize.rs from line 470, settings.rs from line 2372, test_support.rs from line 657) do ship.
- Cadence is the unmeasured cost. v0.10.0 is dated 2026-09-22 and v0.10.1 2026-10-07 (15 days, measured from tags). The sequencer's 10 patch releases to 0.11.0 is about five months at that rate, with no calendar stated. The shared prefix here is 0.10.2 to 0.10.5, roughly six weeks. Track B items are explicit, optional and uncalendared.
- Never widen a trust boundary silently. Every `pub(crate)` to `pub` change (try_lock_file_exclusive, test-support items, execpolicy rule types, credentials) gets a one-paragraph visibility review in the commit message.
- Release-time human gates stay human: tags, crates.io upload, new package names, the publication of codewhale-ratatui. tui's manifest depends on crates.io codewhale-ratatui 0.1.2, which is excluded from the workspace and is not in crates.sh or check-crate-sizes.py. It must already be published before any dry-run or upload that builds tui from a tarball.

## Releases

### 0.10.2: Ship the candidate as-is. No structural move (moved here from the sequencer's X2-in-0.10.2).

Slices:
- Release the stamped candidate. workspace.version is already 0.10.2 in Cargo.toml, and `wip(0.10.2)` and ratatui-0.1.1 release-prep commits are on main. The working tree has 48 dirty entries (CHANGELOG.md, settings.rs, tui/app*, runtime_api/*, core/authority.rs, a staged deletion of full_access_confirm.rs). A structural move on top of an RC with another agent's feature in flight is the largest avoidable risk in the plan.
- Non-move rider: write the real per-crate tarball sizes (from the dry-run's check-crate-sizes.py output) into the release notes table. This is the baseline column for every later release.
- Non-move rider: record the codewhale-ratatui 0.1.2 publication receipt in the release notes (out-of-band crate, not in crates.sh).

- **Entry gate:** Founder-owned release process per docs/RELEASE_RUNBOOK.md. `python3 scripts/split/module_graph.py --check` PASS at 11/32/1/1. codewhale-ratatui 0.1.2 is on crates.io (human gate). The dirty full-access and settings work is either in the candidate or excluded by its owner.
- **Exit gate:** `./scripts/release/publish-crates.sh dry-run` passes for all 29 crates, including check-crate-sizes.py. validate-crate-publish-order.py passes. Hosted CI green on the exact SHA (macOS and Windows jobs). Source, local, CI, packaged-artifact and release-state evidence are reported separately.
- **Ratchet target:** prod 11, test 32 (RS-12 cleared fleet|lib.rs 3), late 1, uilib 1.
- **Size budget:** All 29 crates: no growth versus the recorded table. codewhale-tui about 9.47 MiB real (about 9.50 by proxy). Not a gate on structure.
- **Risks:** Carrying a structural move into the candidate would collide with the dirty tree. Nothing to roll back structurally.
- **Rollback:** Normal release rollback per the runbook. No structural change to revert.

### 0.10.3: Finish RS-13: command catalog port (X2). The only move.

Slices:
- X2 command_catalog leaf in crates/tui/src (new module), per the design doc. runtime_api.rs:4815-4898 (user_registry, command_infos, traits::usage_subcommands, CommandDiscovery, UNLISTED_COMMANDS, with_registry_for_workspace) and tools/tui_help.rs:14,173,210 go through it; extension_host/command.rs per the design table. The port must delete the old direct call sites in the same slice (no second registry).
- Design-doc correction the sequencer dropped: install the real catalog process-wide for tests. A thread-local guard cannot reach the Runtime API test server thread.
- Riders (no graph change): cut `runtime_api|commands` test edges (2) in the same files; move `late` (test_cases_04.rs:1197) and `uilib` (roster_routes.rs:798) test edges only if the owning test files are not dirty.

- **Entry gate:** module_graph.py --check PASS at 11/32/1/1. The files this slice edits are clean in `git status`: runtime_api.rs (13,649 lines, not dirty now), tools/tui_help.rs, commands/catalog.rs, extension_host/command.rs. The sequencer's 'clean in all of commands/ and runtime_api/' would block this slice today, since runtime_api/tests.rs, runtime_api/thread_history.rs and commands/groups/config/config.rs are dirty. Require only the exact files edited, and coordinate on runtime_api/tests.rs if the test guard install touches it. One named build owner.
- **Exit gate:** Slice gate: ratchet --check at prod 0 with the baseline JSON updated in the same commit. cargo fmt --check. clippy with CI's flags. `scripts/dev-test.sh` runtime_api and commands filters with N > 0 passed, including a slash-command-list API test and the /help test. Journey on the built binary: slash palette lists commands, /help, GET of the runtime API command list. Release gate at cut: publish-crates.sh dry-run and green hosted CI on the SHA.
- **Ratchet target:** prod 11 -> 0, test 32 -> 30 (runtime_api|commands 2 cleared if achieved), late 1, uilib 1. Design doc predicts the cycle 100 -> 83 modules; re-run partition_report.py and quote the real number.
- **Size budget:** No crate grows. A new leaf module in tui adds a few KiB.
- **Risks:** Highest churn path: runtime_api is 286 commits in 30 days. The port could be built as a second registry (guard by deleting callers). The catalog process-wide install can leak between tests.
- **Rollback:** Annotated local recovery tag plus a CW/backups note. `git revert` of the slice commit on main. No reset, no force-push. If 0.10.3 is already cut, fix forward before any other move.

### 0.10.4: W0-S2: codewhale-test-support (publish=false), env half only.

Slices:
- New crates/test/test-support, publish = false, version.workspace = true, added to root Cargo.toml `members` (the sequencer omitted the members edit). Move tui/src/test_env_lock.rs (300 lines) and the env half of test_support.rs (non-test part 657 lines: isolated_test_state_root, SealedHome, EnvVarGuard, state_io_lock, guarded_env_keys). That is about 960 lines, not the 1.5k claimed. The tui test_support.rs stays as a shim plus the App fixtures and the fence tests (they name tui::app types).
- Same seam: drop the cfg(test) mount of test_env_lock in shell_dispatcher.rs and the #[path] include in tests/integration/main.rs.
- Rider: cut `test_support|tui` test edges that fall out (7 today).

- **Entry gate:** 0.10.3 shipped. No one else edits test_support.rs, test_env_lock.rs or shell_dispatcher.rs. Confirm the new manifest uses `version.workspace = true`: validate-crate-publish-order.py checks that ALL workspace packages share one version (it counts publish=false crates in that check but not in the publish list). Confirm check-versions.sh item 4 only greps path deps that carry a version, so a version-less path dev-dep passes.
- **Exit gate:** Slice gate: ratchet --check (test <= 33). `cargo test -p codewhale-tui --test integration --no-run`. nextest of a sample of EnvVarGuard users (spawns-binaries group) with equal pass counts before and after, N > 0. Exactly one copy of the process-global lock in the dependency graph, checked by reading Cargo.toml paths (grep for the symbol in crates/*: only tui and the new crate). Release gate: dry-run proves the published tui manifest has the path dev-dep stripped and crates.sh is unchanged at 29. Windows CI green for the cfg(unix) sandbox-visible tempdir code.
- **Ratchet target:** prod 0, test <= 33, late 1, uilib 1 (no change expected from the carve itself).
- **Size budget:** No crate grows. Not a size measure: the moved code is cfg(test) helpers.
- **Risks:** Low. A second copy of the env mutex stops env tests serializing. The 7 other private env-guard copies stay (later cleanup). Why this is on the critical path: settings.rs has 34 test_support refs and credentials tests mutate env, so neither can move to a lower crate without a lower test-support.
- **Rollback:** Recovery tag; revert one commit (3 files plus Cargo.toml and Cargo.lock; shim means consumers were untouched). Cargo.lock regeneration only by the build owner.

### 0.10.5: Test-edge rehoming and the A/B decision release. Non-graph cleanup, no crate moves.

Slices:
- Rehome the remaining test edges so no #[cfg(test)] code in the runtime half names crate::tui, commands or lib.rs: tools|tui 12, client|tui 3, session_manager|tui 3, config|tui 2, core|tui 2, prompts|tui 1, plus late 1 and uilib 1. Mechanism: move each such test to a tui-side integration module or replace the tui fixture with a runtime-side one. Split across several landings on main, owned per directory.
- Record the founder's A/B decision (open_decisions[0]) before any Track B release starts.

- **Entry gate:** 0.10.4 shipped. Per-landing: the test files edited are not dirty. Mapping of the 32 edges to files regenerated with `module_graph.py --report` at the start of the release and posted in the Linear issue.
- **Exit gate:** module_graph.py --check at prod 0, test 0, late 0, uilib 0, baseline JSON updated. Existing tests for each touched file run with identical pass counts (quote before and after). No production file changed except visibility needed by a moved fixture. The A/B decision is recorded in docs/design/TUI_DECONSTRUCTION.md.
- **Ratchet target:** prod 0, test 33 -> 0, late 1 -> 0, uilib 1 -> 0.
- **Size budget:** No change. Test files are excluded from the tarball.
- **Risks:** Test relocation is not rename-dominated and can silently drop coverage: gate on equal before/after pass counts per directory. tools tests are 152k production lines of neighbours, so collisions are likely; land per-directory.
- **Rollback:** Per-landing revert. No published state touched.

### 0.10.6: [Track B only; skipped if Option A] X1: fslock into crates/runtime.

Slices:
- Move try_lock_file_exclusive (runtime_threads.rs:6592) together with RuntimeProcessOwnerLock::try_lock_exclusive and is_contention (runtime_threads.rs:6855) into crates/runtime (already depends on fd-lock 4.0.4). The sequencer listed only the free function. try_lock_file_exclusive calls both associated functions, so a verbatim 'pure rename' is not enough; the design table calls this S, about 190 lines, 'held' (has_live_holder is a fifth call cluster). Removes the session_manager -> runtime_threads lock edge (session_manager.rs:1823, 1879, 1954 production, plus session_reconcile.rs:256).
- Visibility review of every pub(crate) -> pub item recorded in the commit message.

- **Entry gate:** 0.10.5 shipped and Track B chosen. runtime_threads.rs and runtime_threads/tests.rs are committed by their writers (both dirty today; runtime_threads is 169 commits in 30 days).
- **Exit gate:** Slice gate: `cargo test -p codewhale-runtime`; runtime_threads owner-lock and session_manager lease tests with N > 0; clippy as CI. Journey: two sessions in one workspace (second refused or waits as before), resume a saved session. Windows CI green (fd-lock paths). Release gate as usual.
- **Ratchet target:** prod 0, test 0, late 0, uilib 0 (no ratchet movement; module edges only).
- **Size budget:** No crate grows.
- **Risks:** Owner lock is a data-loss boundary. Move verbatim and keep semantics. Visibility widening needs review.
- **Rollback:** Recovery tag; revert immediately on any lock regression (data-loss path), do not patch forward.

### 0.10.7: [Track B only] X7: tool rules and auto-review types into execpolicy.

Slices:
- Per the design table: tool_denied, tool_matches_any_rule, requires_raw_shell and policy_tool_aliases (core/engine/tool_catalog.rs) into crates/execpolicy/src/tool_rules.rs. AutoReviewPolicy lives in core/authority/auto_review (config.rs has 25 AutoReview references). The sequencer's AutoReview* + HumanDecision scope differs from the design table's. Reconcile the two lists before starting; not re-verified here.
- Pure types and functions, re-exports at old paths, no behaviour change.

- **Entry gate:** 0.10.6 shipped. core/authority.rs (dirty today, full-access work) is committed by its writer. execpolicy depends only on protocol (verified), so no cycle.
- **Exit gate:** Slice gate: cargo test -p codewhale-execpolicy; authority, config and mcp approval tests with N > 0; byte-for-byte diff of the moved matching functions. Journey: approval prompt for a shell call and an MCP call, auto-review on and off, receipts unchanged. Release gate as usual.
- **Ratchet target:** no ratchet movement (prod already 0). Real measure: partition_report edge counts for config->core, mcp->core, tools->core.
- **Size budget:** No crate grows.
- **Risks:** Approval trust boundary: no visibility widening, no matching change. Collides with active full-access work.
- **Rollback:** Recovery tag; revert whole commit (re-exports keep consumers compiling only if the revert is whole).

### 0.10.8: [Track B only] X4: schema_sanitize/schema_canonicalize into codewhale-models.

Slices:
- Move only the production half of tools/schema_sanitize.rs (lines 1-469; tests start at 470) and schema_canonicalize.rs (207 lines total) into codewhale-models, which already owns Tool. prepare_tools_for_strict_mode (schema_sanitize.rs:50) is the only user of codewhale_models::Tool, so the destination must depend on models. crates/tools (protocol-only, publishes before config and models in crates.sh) would need a new dependency and a crates.sh reorder. The sequencer's 'look at crates/tools first' is the wrong first look.
- The test modules (schema_sanitize.rs:470 to end, 61 test fns, some using crate::tools::apply_patch::ApplyPatchTool) stay in tui. This is a split test module, not rename-only.
- Cut client -> tools schema refs: client/anthropic.rs, responses.rs, chat.rs, core/engine/tool_catalog.rs, tools/registry.rs, tools/workflow/plan_schema.rs.

- **Entry gate:** 0.10.7 shipped. client/*.rs not dirty and #6396 does not claim those client files. No new package name needed, so no founder name gate.
- **Exit gate:** Slice gate: cargo test -p codewhale-models and the tui schema tests with equal counts before and after, N > 0; assert byte-identical sanitized output on the existing fixtures (KV-cache prefix, docs/CACHE.md). Mock-server strict-schema tests; a live provider call is 'not run' unless the founder approves spend. Release gate as usual.
- **Ratchet target:** no ratchet movement; client -> tools refs fall by the schema refs.
- **Size budget:** No crate grows. Moved production is about 676 lines (roughly 0.006 MiB by proxy), not the 3.2k lines claimed; the 2.3k test lines travel only if the test module moves, and they stay.
- **Risks:** Sanitizer output is request-visible; a changed canonical form breaks the KV-cache prefix. models gains about 0.7k lines.
- **Rollback:** Recovery tag; revert. No crates.sh change, so nothing to unwind in publish order.

### 0.10.9: [Track B only] P1: credentials into codewhale-secrets.

Slices:
- Move crates/tui/src/credentials/ (a directory: mod.rs 138, source.rs 195, store.rs 306, context.rs 63 = 702 production lines, tests.rs 294) into codewhale-secrets. The sequencer wrote 'credentials.rs'. Its one outbound edge, doctor (structural_url_authority), moves down first in the same slice. A different file, crates/config/src/credentials.rs, already exists: resolve the name overlap before choosing the module path.

- **Entry gate:** 0.10.8 shipped. 0.10.4's test-support is in (credentials tests mutate env). credentials/ and doctor.rs are not dirty. codewhale-config already depends on secrets, so secrets cannot depend on config: confirm credentials uses nothing from tui config (the only crate:: ref found outside tests is doctor).
- **Exit gate:** Slice gate: cargo test -p codewhale-secrets for credentials tests with N > 0. Config credential-resolution tests via dev-test.sh. Journey: store, read back, rotate a key; no secret printed in logs. Windows ACL paths compile on hosted CI. Release gate as usual.
- **Ratchet target:** no ratchet movement.
- **Size budget:** No crate grows.
- **Risks:** Secrets trust boundary: file modes, redaction, keyring fallback unchanged. Windows cfg paths cannot be proven locally.
- **Rollback:** Recovery tag; revert (shim means consumers untouched).

### 0.10.10: [Track B only] P2 seam: make LlmClient free of tui types.

Slices:
- Remove BOTH tui-typed default methods from the LlmClient trait (llm_client/mod.rs:106-131): effective_max_output_tokens (calls crate::route_budget::effective_max_output_tokens_for_route) and effective_route_envelope (returns crate::cost_status::EffectiveRouteEnvelope). The sequencer named only the second. Convert both to free functions beside their tui callers and update every client implementor that overrides them. This is a seam slice with no crate moved.
- Point error_taxonomy's ToolError import (error_taxonomy.rs:5) at codewhale_tools::ToolError (crates/tools/src/lib.rs:56 exists and tui spec.rs already imports it from there).
- Move RetryPolicy (config.rs:1649) to codewhale-config.

- **Entry gate:** 0.10.9 shipped. #6396 has released or landed its claim on cost_status, route_budget, provider_lake and pricing (open decision). client/ not dirty.
- **Exit gate:** Slice gate: cargo check -p codewhale-tui; client and llm_client tests with N > 0; clippy. Mock-server 429 and 5xx retry classification and user-visible text unchanged. Release gate as usual.
- **Ratchet target:** no ratchet movement.
- **Size budget:** No crate grows.
- **Risks:** Trait signature change touches every client implementor. Conflicts with the #6396 owner.
- **Rollback:** Recovery tag; revert.

### 0.10.11: [Track B only] P2 move: LlmError, LlmClient and error_taxonomy into codewhale-models.

Slices:
- Move error_taxonomy.rs (1,054 lines), and LlmError, the LlmClient trait, RetryConfig and with_retry from llm_client/mod.rs (2,164 lines, tests at :34, :308, :589) into codewhale-models. with_retry (llm_client/mod.rs:1593) calls error_taxonomy::envelope_for_llm_error, so the two must move together. models depends on config and core (verified), so it can take RetryPolicy and ToolError. Path-alias block via scripts/split/move-modules.py; the 76+44 consumer files do not change because of the shim.

- **Entry gate:** 0.10.10 shipped and the trait is tui-free. Confirm models -> tools adds no cycle: tools depends on protocol only, and crates.sh order (tools before models) already holds.
- **Exit gate:** Slice gate: cargo check -p codewhale-models -p codewhale-config -p codewhale-tui; error_taxonomy and llm_client tests with N > 0; saved-session replay fixture for error rendering with serialized forms unchanged. Release gate as usual.
- **Ratchet target:** no ratchet movement.
- **Size budget:** No crate grows. tui sheds about 3.2k lines (about 0.03 MiB by proxy), not material.
- **Risks:** 120 consumer files. Variant or serialization change breaks session logs ('model-visible means logged').
- **Rollback:** Recovery tag; revert (alias block makes consumer revert one file).

### 0.10.12: [Track B only] X8 seam then contract: split into two releases (0.10.12 S0 free functions, 0.10.13 contract move).

Slices:
- 0.10.12: convert `impl FleetRole` (tools/subagent/mod.rs:639) to free functions beside fleet/role.rs and resolve the duplicate ChildAuthority types. Seam only.
- 0.10.13: move FleetRole, AgentProfile, ProfileOrigin, AgentWorkerStatus and mailbox/result DTOs to a contract home. Prefer an existing crate. A new package needs a founder name that is not codewhale-agent.
- Note: X8 needs X7 first (design table) and is blocked by the untracked crates/tui/src/SUBAGENT and fleet work in the tree.

- **Entry gate:** 0.10.11 shipped. X7 landed (0.10.7). The untracked SUBAGENT/ and fleet/ writers have committed or removed their work. tools/subagent is 226 commits in 30 days: freeze subagent edits.
- **Exit gate:** Slice gate: subagent and fleet tests with filters (subagent tests.rs is 27k lines). Journey: spawn, observe, cancel a subagent; runtime API subagent list. Release gate as usual.
- **Ratchet target:** no ratchet movement; tools|fleet 62 refs fall.
- **Size budget:** No crate grows.
- **Risks:** Most active area besides providers. Orphan rule: the inherent impl must be gone before the type moves.
- **Rollback:** Recovery tag; revert.

### 0.10.14: [Track B only] X6 part 1 only: the DTO subset of core/events.rs and core/ops.rs that is already lower-crate-clean; settings S1 follows as 0.10.15.

Slices:
- Per the design table X6 is S, about 300 lines, not the whole 1,614. events.rs (1,141) imports crate::tools::subagent::{AgentWorkerStatus, CoordinationDetailProjection, SubAgentResult, MailboxMessage, SubAgentStatus, AgentRunUsage, UserFollowUpOutcome}, tools::goal::GoalSnapshot, tools::user_input::UserInputRequest, mcp::McpManagerSnapshot, tool_inspection, snapshot, agent_roster, route_receipt, route_billing, provider_catalog_live and cost_status. ops.rs (473) imports compaction::CompactionConfig, route_runtime::ResolvedRuntimeRoute, hooks::HookExecutor, fleet::roster::FleetRoster, core::engine::preview::PreviewRequestInputs and cost_status::RuntimeUsageBatch. None of those has a seam in this plan. Move only the self-contained subset and leave the rest until X8, #6396 and the engine move.
- 0.10.15 (settings S1): settings.rs production is 2,372 lines (cfg(test) at :2372), not 2.9k, and imports pricing (3 refs), reasoning_preference and config. It is blocked by #6396 for CostCurrency and needs test-support (34 refs). Move the pure core only, with a persisted-settings compatibility fixture.

- **Entry gate:** 0.10.13 shipped. core/events.rs and UI handlers not dirty. No new Event or Op variants in flight. For 0.10.15: settings.rs not dirty (dirty today), #6396 resolved.
- **Exit gate:** Slice gate: protocol_parity and runtime_api event tests; wire format byte-identical (ACP, SSE, TUI turn journeys). For settings: load a 0.10.x settings file unchanged; /settings round trip. Release gate as usual.
- **Ratchet target:** no ratchet movement; closure modules 117 -> 116 for settings.
- **Size budget:** No crate grows.
- **Risks:** Event enum is a persisted and over-the-wire contract. Settings is a persisted compatibility contract.
- **Rollback:** Recovery tag; revert immediately on any wire or settings-format change.

### 0.11.0: Decision release. Option A (recommended placement): RS-14 atomic move of the remaining runtime SCC. Option B: Config data sections continue.

Slices:
- Option A (RS-14, #6941): after 0.10.5, move the remaining runtime half into one new crate with scripts/split/move-modules.py, everything except src/tui, src/commands, composer_*, model_relevance and config_keys; codewhale-tui stays the UI plus composition root. Proxy sizes: UI 2.78 MiB (435 files), runtime remainder 6.89 MiB (714 files, of which extension-host, plugins and assets 1.16 MiB). The sequencer's 2.7 / 6.7 estimates are within 0.2 MiB.
- Compatibility event, corrected: crates/cli uses codewhale_tui::run (11 uses), RuntimeOptions (8 uses, ::default and the type), route_preferences (get, is_route_key, set_document, set, selected_route, unset, scrub_root_model_aliases_for_export), config_keys (resolved_stream_config, unread_config_keys, config_key_home) and set_telemetry_preference. The sequencer's list omitted RuntimeOptions and set_telemetry_preference. Keep all re-exported from codewhale-tui.
- Publish impact: crates.sh goes 29 -> 30 (new crate placed after core, config, models, runtime, before tui). codewhale-ratatui 0.1.2 stays out of band, so 'all 30 crates' is 30 plus ratatui.
- Option B (founder declines RS-14): continue Track B items and then config-types sections one per release. tui stays about 9.5 MiB and the cap stays the binding constraint.

- **Entry gate:** Option A: 0.10.2 to 0.10.5 shipped. Ratchet reads prod 0, test 0, late 0, uilib 0. Founder A/B decision recorded and new crate name approved (human gate). Frozen write window across the workspace and one build owner (a new crate adds a build cache: df -h /Volumes/VIXinSSD first). Dirty tree empty for crates/tui. Option B: Track B through 0.10.15 shipped.
- **Exit gate:** Option A: `cargo check --workspace`, clippy as CI, nextest for the moved crate and tui UI tests, `scripts/dev-test.sh` areas. publish-crates.sh dry-run over all 30 crates with both tui and the new crate under 9.0 MiB (no warning). Build from the unpacked tarball to catch include/exclude gaps (extension-host/dist, plugins/computer-use assets; include_bytes! cannot cross a package root; build.rs macOS clang step must move with the include_bytes user). Journeys from the built release binary: onboarding, agent turn with tool call, subagent spawn, MCP connect, resume, runtime API/app-server, ACP, Windows startup. Hosted CI green on macOS and Windows for the exact SHA.
- **Ratchet target:** Option A: ratchet retired (becomes a compile error). Option B: all four counts stay 0.
- **Size budget:** Option A: tui <= 3.1 MiB and new runtime crate <= 7.2 MiB by the same proxy, both under the 9.0 MiB warn line. Option B: tui no growth (about 9.5 MiB).
- **Risks:** Highest in the plan. A whole-workspace rename collides with every writer; mitigated by the freeze, mechanical tooling and the 0.10.3 to 0.10.5 prerequisites. The design doc rejects 'move the monolith' as an end state, so A must be recorded as an intermediate step with its own known-limitations note.
- **Rollback:** Recovery tag plus CW/backups bundle. One commit, revert as one commit. No reset, no force-push. A revert cannot unpublish crates.io uploads, so the move is dry-run complete before any upload and publication stays a founder action.

## Measurement plan

Record at every release, on one machine, at a named source commit with a dirty-file inventory. Numbers from a tree that changed during the compile are void. (1) Tarball sizes: the real sizes from the release dry-run's check-crate-sizes.py output for all published crates, in a table with the previous release's value and the delta. Between releases use the proxy, run from crates/tui: `git ls-files . | grep -vE '^tests/|/tests\.rs$|/tests/|_tests\.rs$|/test_cases_[^/]*\.rs$|^extension-host/test/|^plugins/computer-use/tests/' | tar -czf /tmp/x.tgz -T -`. It read 9.50 MiB for HEAD against 9.47 real, so it is good to about 0.03 MiB. Anything smaller than 0.03 MiB is not measurable, so no per-slice size budgets below that. Mark each cell proxy or real. (2) `python3 scripts/split/module_graph.py --check` four counts plus the closure-module count, committed as the baseline in the same commit as the move. Today: prod 11, test 32, late 1, uilib 1, 117 modules. (3) Build timing via scripts/dev-cargo.sh after `df -h /Volumes/VIXinSSD`: cold `cargo test -p codewhale-tui --lib --locked --no-run --timings`; warm touch-one-file loops; a loop touching a file inside the moved code; peak RSS via /usr/bin/time -l; three runs, median and spread. Baseline from docs/BUILD_PERFORMANCE.md: cold 94 s for `cargo build -p codewhale-tui`, about 12.5 s incremental build and about 20 s lib-test. No speedup is claimed for any leaf move (each is under 1% of the closure). Only Option A can plausibly change build time, and it is not measured until run. (4) nextest wall time and real 'N passed' per affected crate (N > 0), quoting before and after for every relocated test. (5) Hosted CI job durations for macOS and Windows on the exact SHA. Windows-only cfg code (test-support tempdir, credentials ACLs, fd-lock paths) cannot be proven locally. (6) Churn: `git log --since=30.days` per moved path. Today runtime_api is 286, runtime_threads 169, tools/subagent 226, settings.rs 16, core/events.rs 27. Use it to schedule landings, not to justify moves. (7) Release cadence: days between tags (v0.10.0 to v0.10.1 was 15 days) recorded per release to keep the calendar honest. (8) One table row per release in docs/design/TUI_DECONSTRUCTION.md, each cell marked measured, proxy or not measured.

## Coordination rules (shared checkout)

- Shared checkout, partitioned by file: before a slice starts, the owner posts in the Linear tracking issue the slice, the exact paths claimed, the build owner and the freeze window. Everyone else treats those paths as read-only until the landing commit. Claim exact files, not directories. The sequencer's 'clean in commands/ and runtime_api/' would have blocked X2 on unrelated dirty files.
- Commit by pathspec, not by index. `git status` right now shows a pre-staged deletion (`D  crates/tui/src/tui/full_access_confirm.rs`) that belongs to another agent. A bare `git commit` after staging your own paths would sweep it into your commit. Use `git commit -- <owned paths>` or check `git diff --cached --name-only` against the owned list before every commit. Never `git add -A`. Retry index.lock.
- Dirty tree is the entry gate for specific files, not for the whole release: today 48 entries incl. settings.rs, core/authority.rs, runtime_threads.rs, runtime_threads/tests.rs, runtime_api/tests.rs, tui/app*, untracked crates/tui/src/SUBAGENT and fleet. Each Track B release names the dirty files it needs, and cannot start until their owner commits or hands off, noted in Linear. If a needed file stays dirty beyond one release window, skip that release and take the next independent one (X1, X7, X4 and P1 are independent).
- One native build per machine, one build owner per release. Only the build owner edits Cargo.toml (root members, crates/tui/Cargo.toml), Cargo.lock and scripts/release/crates.sh. Batch every edit, then compile once. Record source commit plus dirty-file inventory next to each result.
- Two gate tiers (see principles). Slice gate on landing to main; release gate at the cut. Landing main uses a fetch, rebase onto origin/main, a retry on rejection, no force-push. Hosted CI on the landing SHA is a separate evidence level from local tests. The release cut waits for it, since main lands before CI runs.
- Freeze windows apply to a claimed tree for the length of the slice: no new modules, no Event or Op variants (for the events slice), no edits to claimed files by peers. Contributor PRs still merge as PRs, and a maintainer rebases a contributor PR that conflicts with a claimed path. Never ask the contributor to rebase around our churn.
- Delegated workers (Claude, muse, Grok) get read-only discovery or disjoint file ownership and a time and cost limit, are told to commit before multi-file work, and are never given branches, checkouts, publication or provider spend. Their reports are self-reports. `git status` and the diff are the evidence.
- Authority: tags, crates.io upload, new package names, codewhale-ratatui publication, DNS and billing stay human. Recovery tags are local and logged in CW/backups; pushing them is a founder call. No agent comments on GitHub issues or PRs: evidence goes in commits, claims in Linear (CW team at linear.app/shannonlabs; check get_workspace first because shannon-labs is retired). Linear-to-GitHub mirroring is not re-established, so say so in each issue. Keep inference-business detail out of public trackers.
- Handoff: report canonical path, branch, commit, upstream state and leftover dirty work, and refresh WORKSPACE_STATUS.md after topology changes.

## Open decisions and what the critic changed

- CHANGED: moved the A/B decision (RS-14 atomic move vs the design doc's slice-only path) from 'decide by 0.10.9' to 'record in 0.10.5, before any Track B release'. Reason: Option A makes X1, X4, X7, X8, P1, P2 and settings non-prerequisites, because they shrink a cycle that moves whole. Under A the work is 0.10.2 to 0.10.5, then 0.11.0, about six weeks at the measured 15-day cadence. Under B it is about fourteen releases. The sequencer paid for B and offered A as an afterthought. Size is also evidence: only A moves tui below 9.0 MiB (proxy UI 2.78 MiB, runtime 6.89 MiB); every leaf frees about 0.01-0.03 MiB. Consequence of A: library path break risk for codewhale-tui (keep re-exports of run, RuntimeOptions, route_preferences, config_keys, set_telemetry_preference) and a 30th crate. Consequence of B: tui stays at about 9.5 MiB for months with 0.5 MiB of headroom.
- CHANGED: 0.10.2 now ships the candidate with no move, and X2 became 0.10.3. Reason: Cargo.toml already stamps 0.10.2 and the tree holds in-flight full-access, settings and runtime_api work from other agents. A structural move on a candidate is unreviewable risk. Founder to confirm that 0.10.2 is frozen. If it is not frozen, X2 may go back into it.
- CHANGED: entry gate for X2 relaxed from 'git status clean in commands/ and runtime_api/' to 'the exact edited files are clean'. Verified: runtime_api/tests.rs, runtime_api/thread_history.rs and commands/groups/config/config.rs are dirty now, while runtime_api.rs (the file holding the 9 prod refs at :4815-4898) and tools/tui_help.rs are not. Also added the design doc's requirement to install the real catalog process-wide for tests, which the sequencer dropped.
- CHANGED: added 0.10.5 (test-edge rehoming plus late/uilib) which did not exist. Reason: the sequencer's 0.11.0 entry gate required test <= 34, but no release lowered any of the 35 test edges (test_support|tui 7, tools|tui 12 etc.), and keeping late 1 and uilib 1 would put crate::exec_agent and crossterm references into a crate that Cargo.toml says must never depend on tui or crossterm. Rehoming is test-only, not a graph change, so it can ride in pieces.
- CHANGED: test-support (now 0.10.4): LOC corrected to about 960 (657 non-test lines in test_support.rs plus 300 in test_env_lock.rs), not 1.5k. Added the root Cargo.toml members edit and `version.workspace = true`. Verified in validate-crate-publish-order.py: the single-version check iterates all workspace packages, including publish=false ones, so an off-version test-support breaks the release gate. Added the reason this is on the critical path (settings.rs has 34 test_support refs; credentials tests mutate env).
- CHANGED: X4 destination is codewhale-models, not crates/tools and not a new util-schema package. Verified: schema_sanitize.rs:50 prepare_tools_for_strict_mode uses codewhale_models::Tool, crates/tools depends only on protocol, and crates.sh publishes tools before config and models. Moved production is about 676 lines (schema_sanitize.rs tests start at line 470 and use tui-only ApplyPatchTool), not 3.2k. So the move is a split test module, not rename-only, and it needs no founder name approval. If the founder insists on a util crate, this reverts to the sequencer's plan.
- CHANGED: P2 split into a seam release (0.10.10) and a move release (0.10.11). Verified the LlmClient trait has two tui-typed default methods (effective_max_output_tokens via route_budget at llm_client/mod.rs:109 and effective_route_envelope via cost_status at :123), not one; with_retry (:1593) calls error_taxonomy::envelope_for_llm_error so they move together. ToolError is already re-pointable (crates/tools/src/lib.rs:56). The sequencer's 'split if large' hedge is now the plan.
- CHANGED: X6 reduced to the design table's part 1 (about 300 lines), not the full 1,614-line events.rs+ops.rs move at 0.10.11. Verified events.rs imports seven tools::subagent types, GoalSnapshot, UserInputRequest, McpManagerSnapshot, tool_inspection, snapshot, agent_roster, route_receipt, route_billing, provider_catalog_live and cost_status; ops.rs imports compaction, route_runtime, hooks, fleet::roster, engine::preview and cost_status. The sequencer's plan puts X6 before those seams exist. A sequencing error.
- CHANGED: settings S1: production is 2,372 lines (first cfg(test) at settings.rs:2372), not about 2.9k, and the file imports pricing, so it stays blocked on #6396 for CostCurrency. Moved to the end of Track B (0.10.15).
- CHANGED: X1 scope: the move must also take RuntimeProcessOwnerLock::try_lock_exclusive and is_contention (runtime_threads.rs:6855). The sequencer described a pure rename of the free function (runtime_threads.rs:6592) that calls both. Verified production call sites: session_manager.rs:1823, 1879, 1954 and session_reconcile.rs:256; the other five are tests. Marked seam-introducing, not pure rename.
- CHANGED: size gates. Removed the per-slice size_budget numbers (9.44, 9.43, 9.42, 9.38, 9.36, 9.34, 9.30) and the per-slice `cargo package` + check-crate-sizes gate. They sit below proxy resolution (about 0.03 MiB) and a single-crate `cargo package` cannot resolve unpublished 0.10.x workspace deps; only the multi-package dry-run can, and it builds all 29 tarballs. Replaced with: no crate grows at the release cut, plus one proxy command for between-release checks. Size-guard question for the founder: keep the 10 MiB hard fail unchanged (recommended); decide whether the fixed 90% warn becomes relative to the last release's recorded size. The warn fires on codewhale-tui every release at 94.7% and is the only noise. Not changed by this plan, and I recommend leaving the script alone until the table of real sizes exists.
- CHANGED: publish-side facts. codewhale-ratatui 0.1.2 is a published dependency of tui that is neither in crates.sh nor checked by check-crate-sizes.py (vendor/codewhale-ratatui is a workspace exclude; check-versions.sh special-cases it). It must be on crates.io before any dry-run that unpacks tui. Added to the 0.10.2 entry gate and to the 0.11.0 'all 30 crates' wording. Founder to confirm that its size is intentionally outside the guard.
- CHANGED: coordination. Added pathspec commits. Verified `D  crates/tui/src/tui/full_access_confirm.rs` is already staged by another agent, so 'stage only owned paths' is not enough if the commit takes the whole index. Replaced the blanket 'dirty tree blocks the slice' with a named-files rule and a skip-to-next-independent-release rule.
- CHANGED: 0.11.0 compatibility list: crates/cli also uses RuntimeOptions (8 uses) and set_telemetry_preference. Proxy sizes validate the sequencer's Option A estimate within 0.2 MiB (UI 2.78 vs 2.7, runtime 6.89 vs 6.7); the budget is raised to 7.2 MiB so a measured 6.89 plus growth does not trip it.
- NOT RE-VERIFIED: X7's exact moved set (design table says tool_denied, tool_matches_any_rule, requires_raw_shell, policy_tool_aliases; sequencer says AutoReview* types and HumanDecision), the 9.47 MiB real tarball (only the proxy 9.50 was computed), live state of #6937-#6941, the 12/55/30/62 edge counts in the sequencer's ratchet targets, the 29-crate dry-run build time, any build-time effect.
- Founder decision still open from the sequencer: whether #6396 (catalog and pricing single authority) releases cost_status, route_budget, provider_lake and pricing in time for 0.10.10 and 0.10.15. Under Option A this stops mattering for 0.11.0.
- Founder decision still open: whether the tui exclude list may drop more fixtures or whether extension-host/dist (0.76 MiB compressed proxy) and plugins/computer-use (0.23 MiB) can be fetched at build time. Cheap size lever, but a packaging and offline-build contract change, so a decision, not a slice. Under Option A it is moot.

## Tracking issues to file (one per slice)

- **0.10.5**: A/B decision: RS-14 atomic move vs slice-only path (record in 0.10.5). Decide whether 0.11.0 is the atomic move of the remaining runtime SCC (Option A) or the design doc's slice-only path (Option B). Evidence: with A, X1/X4/X7/X8/P1/P2/settings are not prerequisites; only A moves tui below 9.0 MiB (proxy UI 2.78 MiB / runtime 6.89 MiB). Needs prod/test/late/uilib all 0 first. Linear only, not mirrored. Reuse the #6941 issue if it covers this.
- **0.10.2**: 0.10.2: ship candidate with no structural move; record per-crate tarball table and ratatui 0.1.2 receipt. No move in the candidate. Add real per-crate tarball sizes to the release notes (baseline for later releases) and the codewhale-ratatui 0.1.2 publication receipt (out of band, not in crates.sh). Gate: publish-crates.sh dry-run, validate-crate-publish-order.py, hosted CI green on the SHA.
- **0.10.3**: 0.10.3 X2: command catalog port, finish RS-13 (#6940). runtime_api.rs:4815-4898 and tools/tui_help.rs go through a command_catalog leaf; delete the direct call sites; install the real catalog process-wide for tests. Ratchet prod 11 -> 0. Claim exact files only (runtime_api.rs, tools/tui_help.rs, commands/catalog.rs, extension_host/command.rs). Linear only, not mirrored.
- **0.10.4**: 0.10.4 W0-S2: codewhale-test-support (publish=false), env half. Move test_env_lock.rs (300) and the env half of test_support.rs (657 non-test lines) to crates/test/test-support; version.workspace=true; add to root members; keep tui shim and App fixtures; remove shell_dispatcher cfg(test) mount and the tests/integration #[path] include. Critical path for settings and credentials tests. Gate: equal before/after pass counts, one process-global lock copy, crates.sh stays 29.
- **0.10.5**: 0.10.5 Test-edge rehoming: test 32 -> 0, late 1 -> 0, uilib 1 -> 0. Rehome tests naming crate::tui, commands or lib.rs (tools|tui 12, client|tui 3, session_manager|tui 3, config|tui 2, core|tui 2, prompts|tui 1), plus test_cases_04.rs:1197 (exec_agent) and roster_routes.rs:798 (crossterm). Test-only; equal pass counts before and after per directory. Prerequisite for any new runtime crate that must not depend on tui or crossterm.
- **0.10.6**: 0.10.6 X1 [Track B]: fslock into crates/runtime. Move try_lock_file_exclusive plus RuntimeProcessOwnerLock::try_lock_exclusive/is_contention (runtime_threads.rs:6592, 6855) to crates/runtime. Seam-introducing, not pure rename. Blocked until runtime_threads.rs and its tests are committed. Visibility review in the commit message; revert immediately on any lock regression.
- **0.10.7**: 0.10.7 X7 [Track B]: tool rules into execpolicy. Reconcile the moved set (design table: tool_denied, tool_matches_any_rule, requires_raw_shell, policy_tool_aliases; AutoReviewPolicy lives in core/authority/auto_review) then move to crates/execpolicy with re-exports. Approval trust boundary: no matching or visibility change. Blocked by dirty core/authority.rs.
- **0.10.8**: 0.10.8 X4 [Track B]: schema_sanitize/schema_canonicalize production half into codewhale-models. Move schema_sanitize.rs lines 1-469 and schema_canonicalize.rs to codewhale-models (owns Tool). Tests stay in tui. No new package name. Assert byte-identical sanitized output on existing fixtures (KV-cache prefix).
- **0.10.9**: 0.10.9 P1 [Track B]: credentials/ directory into codewhale-secrets. Move crates/tui/src/credentials/ (702 prod lines, 294 test) to codewhale-secrets after moving doctor::structural_url_authority down. Resolve overlap with crates/config/src/credentials.rs. No secret printed, no file-mode or redaction change; Windows CI green.
- **0.10.10**: 0.10.10 P2 seam [Track B]: remove tui-typed defaults from LlmClient. Remove effective_max_output_tokens (route_budget) and effective_route_envelope (cost_status) from the trait, update implementors, re-point ToolError to codewhale_tools, move RetryPolicy to codewhale-config. No crate moved. Blocked on #6396 for cost_status/route_budget.
- **0.10.11**: 0.10.11 P2 move [Track B]: error_taxonomy + LlmError/LlmClient/RetryConfig/with_retry into codewhale-models. After the seam, move error_taxonomy.rs (1,054) and llm_client/mod.rs (2,164) together via move-modules.py. 120 consumer files via alias/shim. Serialized forms and session logs unchanged.
- **0.10.12**: 0.10.12-0.10.13 X8 [Track B]: FleetRole free functions, then agent-contract move. 0.10.12: convert impl FleetRole (tools/subagent/mod.rs:639) to free functions and resolve the duplicate ChildAuthority. 0.10.13: move the DTO contract (prefer an existing crate; name must not collide with codewhale-agent). Blocked by untracked SUBAGENT/ and fleet/ work and by X7.
- **0.10.14**: 0.10.14-0.10.15 X6 part 1 and settings S1 [Track B]. X6: move only the lower-crate-clean subset of events.rs/ops.rs (~300 lines per design); the full files wait for X8, #6396 and engine seams. Settings: 2,372 production lines, blocked by #6396 (CostCurrency) and needs test-support; persisted settings compatibility fixture required.
- **0.11.0**: 0.11.0 RS-14 (#6941) Option A or Option B continuation. Option A: atomic move of the runtime remainder (proxy 6.89 MiB) into a new crate; tui stays UI (proxy 2.78 MiB). Entry: prod/test/late/uilib all 0, founder decision and crate name recorded, workspace freeze. Exit: 30-crate dry-run with both tarballs under 9.0 MiB, build from the unpacked tarball, journeys from the built binary, hosted CI green. Keep re-exports for run, RuntimeOptions, route_preferences, config_keys, set_telemetry_preference. Option B: Track B continues.
- **0.10.2**: Release tooling: keep the 10 MiB hard fail; decide whether the 90% warn becomes relative. No gate change proposed. check-crate-sizes.py runs only in publish-crates.sh (dry-run and publish), costs nothing extra because the dry-run already builds the tarballs, and caught the 0.10.1 problem. Open: the fixed 90% warn fires on codewhale-tui every release (94.7%). Decide after the real size table exists. Also record that codewhale-ratatui is outside crates.sh and the guard.
