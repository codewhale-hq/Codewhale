# TUI/runtime deconstruction roadmap

The full decomposition is admitted in 0.10.3. The former 0.10.4–0.10.16
release schedule no longer determines scope or prerequisites. [TUI_DECONSTRUCTION.md](TUI_DECONSTRUCTION.md)
owns the target and architecture; [issue #5316](https://github.com/codewhale-hq/Codewhale/issues/5316)
owns the active program. This page orders structural work rather than creating
another task queue.

## Structural order

| Step | Ownership change | Evidence required |
| --- | --- | --- |
| Establish the baseline | Read the actual module graph, production-line count, package contents and current consumers. | Exact source identity and measurement method; distinguish production lines, file lines, SCC size, package size and build time. |
| S35: thread store and manager | Keep durable records/store binding in `runtime_threads`; move manager execution and its history operations to `runtime_threads::manager`. | Migrate every caller; preserve delete/parent-thread, replay/live handoff, approvals and MCP lifecycle. Surface the remaining store-to-manager edge. |
| S52: terminal kit | Move rendering/text helpers into `codewhale-tui-kit`, with direct imports from every consumer. | Preserve streaming block identity, Vim/paste behavior, width/Unicode, links, input routing and existing paint checks; use current dependency pins. |
| N13: error taxonomy | Move shared error categories/envelopes into `codewhale-error-taxonomy`; retain the LLM adapter with its actual client owner. | Preserve exhaustive classification, serialization, stop causes and client-visible errors; remove the old owner after migrating its last consumer. |
| Remaining RS/S/N ownership cuts | Use the measured graph to separate runtime, commands, protocols, tools, configuration, provider/pricing and UI responsibilities. | Complete each consumer migration and retain existing behavior, security and data-loss handling. A saved patch is a proposal until reconciled and verified. |
| Complete the target | Resolve the remaining strongly connected ownership boundaries and oversized crates. | Meet the owning architecture's limits across all crates; a few extracted leaves or a renamed monolith do not complete the program. |

Read Git and the owning issue for S35/S52/N13 status. Older passing VM receipts
qualify their named source only. Saved proposals do not replace current Vim, reasoning,
compaction, thread-deletion or protocol work that landed since they were written.

## Runtime boundaries

Keep one Engine turn loop, session/store authority, event history, base prompt,
permission and credential authority. Clients render and supervise that Engine.
Shared contracts belong below their consumers; rendering dependencies stay on
the UI side of the boundary.

An extraction moves real ownership and its final consumer. Do not add a facade,
re-export layer or second implementation merely to keep old callers unchanged.
A whole-monolith relocation is not the completed architecture. Preserve actual
external contracts explicitly and migrate affected consumers together.

Permission matching, human decisions, cancellation, credential custody, replay
and data-loss guards are not optional during a mechanical move. Keep blocking
work off async/UI threads through the existing runtime facilities.

## Measure and verify each coherent slice

Use the existing tools:

- `scripts/split/count_lines.py` records production/file/test lines.
- `scripts/split/module_graph.py` and its ratchet expose runtime/UI edges.
- `scripts/split/closure_report.py` and `partition_report.py` inform ownership cuts.
- `scripts/dev-test.sh` selects existing checks for the changed owner.
- `scripts/release/crates.sh` owns the actual publication order; derive package
  inventory and versions from the candidate instead of copying historical counts.

Freeze compiler inputs with one build owner, use the shared Cargo wrapper and
record exact source, command, counts and limits. Select checks for the moved
behavior and its callers; no new tests or code comments are implied by this plan.
Do not run exhaustive/hosted validation between slices. Final integrated
qualification belongs to the completed immutable candidate, with only invalidated
checks repeated after a fix.

Package contents, unpacked builds, tarball limits, supported OSes, provider runs,
installed journeys and publication are separate evidence. Local helper tests do
not establish all of them. New crates must appear in the real release order and
versioning consumers before candidate qualification.

## History and recovery

The [pre-cutover roadmap](https://github.com/codewhale-hq/Codewhale/blob/62f0cc9536dcb50d124b81620382a12394aa0f33/docs/design/DECONSTRUCTION_ROADMAP.md)
retains the old release ladder and alternatives as dated rationale. Its future
version gates, fixed crate counts, recovery tags and blanket per-slice CI rules
are not current execution instructions.

Keep canonical development on main and preserve recoverable source/diffs.
Reverse a landed change with an ordinary reviewed revert when needed; do not
reset shared work, rewrite history, delete checkouts or create another integration
lane. Source readiness does not authorize publishing, signing, deployment or
spending.
