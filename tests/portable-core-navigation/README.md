# Core navigation normal-library proof

This unpublished workspace package mounts the **actual production navigation
root**, all ten leaf modules and all 13 registrations. It contains no substitute
App, copied implementation, test-only root or host adapter. Shared metadata,
results/actions, facets and nested payloads are normal dependencies.

Run sequentially from the repository root:

```sh
python3 -B scripts/test_check_command_core_navigation_proof.py
sh scripts/check-portable-core-navigation.sh
```

The proof checks the declared complete source closure and real host inventory
relative order (without pinning unrelated core entries), an all-feature/all-target **normal** dependency graph, a no-default
normal-library build, and nonzero tests from the actual portable leaves.
Rejection controls cover omitted/reordered registrations, metadata drift,
selected-leaf/stub/test-only wrappers, conditional production modules and
unreviewed include macros, host imports (even in an omitted owned leaf), test-only
dependency substitution and unapproved transitive packages.

The approved graph is deliberately explicit: command-contract/protocol data;
serde/JSON/indexed containers and their derive/parser support; chrono/UUID and
existing platform/timezone/random primitives, including their wasm/Windows
support. These are inherited shared data dependencies, not permission to add
TUI, application runtime, configuration, HTTP, terminal or credential-storage
services. New normal packages require source review, not automatic approval.

Host registration, concrete filesystem/view/config effects, real-App tests and
voice recording/transcription remain outside the movable slice. The rest of core
is not claimed portable by this package: the top-level `core` frontier remains
pending for its other 27 registrations at adoption. Physical movement is a later feature.

## Developer and CI coverage

`sh scripts/dev-test.sh crates/tui/src/commands/groups/core/links.rs` (and the
other command source paths) runs the native `commands::` tests, including the
relocated host tests. The `portable-core-navigation` area or proof-package paths
select the fast portable tests; they do not replace host verification.
The heavy Lint job checks the source inventory and normal graph on main as well
as PRs; the Ubuntu PR test job additionally runs the full standalone proof above.
Dependency additions still require explicit review: tolerating unrelated registry
insertions must not silently admit runtime or storage services into the closure.

## Capability limitations

`NAVIGATION` and `VOICE_INPUT` occupy the last two bits of the existing `u32`.
No free bit remains. A future capability must first make an explicit, reviewed
representation/scoping decision; do not silently widen or renumber identities.

The current `WORKSPACE` facet includes confined anchor read/append/replace,
path inspection and home-directory resolution, not just path reads. Other
first-party WORKSPACE consumers therefore have access to those operations even
though they do not call them. Exact facet selection is not per-operation least
authority. Splitting anchor authority is a separate contract decision; this
adoption preserves the agreed workspace facet and does not claim that narrowing
has already been implemented.
