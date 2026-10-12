# Build and test performance

> 阅读简体中文版：[zh_hans/BUILD_PERFORMANCE.md](zh_hans/BUILD_PERFORMANCE.md)。

Measured facts about how long Codewhale takes to build and test, what was
changed to make the contributor loop faster, and what is deferred. Numbers
are from one machine (Apple Silicon, 14 cores, rustc 1.97.0, Xcode 26.2
`ld-1230`) taken while four other cargo jobs were running (1-minute load
average 10–27, recorded next to each number), so treat them as relative
before/after evidence, not benchmarks.

> Split plan: [TUI deconstruction](design/TUI_DECONSTRUCTION.md) contains the
> current source audit, target crate map and extraction order. Measurements
> below are historical; the B3/deferred candidate lists are not an execution queue.

## Where the time goes (baseline, commit 533c530b)

| Step | Wall | Notes |
| --- | --- | --- |
| Cold `cargo build -p codewhale-tui` (empty target) | 94 s (user 270 s) | 543 units; `codewhale-tui` alone is 70 s and is the critical path; next longest units are `codewhale-config` 7.5 s, `jsonschema` 6.3 s, `codewhale-workflow` 5.6 s, `tokio` 5.4 s. Load 13. |
| Cold `cargo test -p codewhale-tui --lib --no-run` (empty target) | 148 s (user 347 s) | The 10.5k-test unit binary is 357 MB and trips the macOS linker's `__eh_frame > 16MB` compact-unwind warning (harmless). Load 20. |
| Incremental `cargo build -p codewhale-tui` after a one-line edit | 12.5 s | Load 11. |
| Incremental `cargo test -p codewhale-tui --lib --no-run` after a one-line edit | 19 s | Load 11. |
| `cargo test --workspace --all-features --locked --no-run` with deps warm | 155 s (user 366 s) | 61 test binaries. Load 6→11. |
| Cold-ish `cargo check --workspace --all-targets --locked` (deps built) | 82 s | Load 19. |
| Running the tui unit suite with libtest (`cargo test -p codewhale-tui --lib`) | 268 s | From the release gate log; 10,531 tests. Load ~10. |
| Running the same suite with `cargo nextest run -p codewhale-tui --lib` | 96–108 s | Same tests, one process per test, all cores busy. Load 12–20. |
| Running the whole workspace under `cargo nextest run --workspace --all-features` | 353 s | 12,744 tests, PTY suite serialized by the nextest config. Load 17. |

Structural facts behind those numbers:

- `crates/tui` is ~746k lines of Rust (609k non-test, 137k inline tests in
  488 `#[cfg(test)]` modules, 10.6k `#[test]`/`#[tokio::test]` functions).
  It compiles as one crate, so the frontend of that crate is the critical
  path of every build and every unit-test run recompiles it with
  `cfg(test)`.
- Dependencies are already trimmed (`reqwest` rustls-no-provider, `image`
  png only, `syntect` default-fancy, `rmcp` no default features, `mimalloc`
  no default features). `cargo tree -d` shows only routine duplicates
  (`toml` 0.8/1.1, `thiserror` 1/2, `strum` 0.27/0.28, `syn` 2/3,
  `sha2` 0.10/0.11) that come from third-party crates, not from workspace
  choices. The global allocator is mimalloc by default; the off-by-default
  `rusty-alloc` cargo feature on `codewhale-tui`/`codewhale-cli` swaps it for
  the pure-Rust `rusty_alloc` remake (#5872). Use
  `cargo build -p codewhale-cli --no-default-features --features rusty-alloc`
  (or `-p codewhale-tui`) to exclude mimalloc and its C build dependency.
  Cargo features are additive: `--features rusty-alloc` alone retains the
  default mimalloc dependency even though the Rust allocator handles allocations.
  This removes the allocator's C build path; other native dependencies may still
  require a C toolchain. With neither allocator feature, the standard library
  system allocator is used.
- `[profile.dev] debug = "line-tables-only"` is already set (#5246) and
  Cargo already uses `split-debuginfo = unpacked` on macOS.
- `target/debug` grows past 50 GB only through accumulation across
  feature sets and worktrees; a fresh test build is ~7 GB.

## A0 receipts (commit 533c530b + hermeticity fixes; empty target dir)

`CARGO_TARGET_DIR=/Volumes/VIXinSSD/CW/.tmp/compile-speed-baseline`, HTML
timing reports archived under
`backups/compile-speed-evidence-20260815/` (a0-cold-lib-test-timing.html,
a0-incremental-lib-test-timing.html, a0-llvm-lines-top40.txt).

| Receipt | Wall | Load (1 min) |
| --- | --- | --- |
| Cold `cargo test -p codewhale-tui --lib --locked --no-run --timings` | 127 s (user 329 s) | 8.9 |
| `touch crates/runtime/src/elapsed.rs` + same command | 21 s | 12.3 |
| `touch` + `cargo test -p codewhale-tui --lib --locked elapsed::` (the everyday loop) | 20 s (4 tests run) | 11.2 |
| Lib-test binary size | 357 MB (`codewhale_tui-<hash>`); links with the `__eh_frame section too large (max 16MB)` compact-unwind warning | — |
| `cargo check -p codewhale-tui --lib --tests` incremental after `touch` (frontend only) | 14 s | 6.2 |
| Incremental full lib-test build after `touch`, same conditions | 28 s | 6.2 |

Cold timing report, top units (605 units): `codewhale-tui` lib test
**106.0 s**, `codewhale-config` 7.9 s, `jsonschema` 5.8 s, `moxcms` 4.7 s,
`codewhale-protocol` 4.2 s, `tokio` 4.0 s, `rustls` 3.8 s, `schemaui`
3.4 s, `rmcp` 3.4 s, `h2` 3.3 s, `jsonschema` (second copy) 3.2 s,
`codewhale-workflow` 3.2 s, `syn` 3.1 s, `rio-vt` 3.1 s, `regex-automata`
3.0 s. The incremental report has exactly one non-zero unit: `codewhale-tui`
lib test 20.8 s. So the everyday tax is the tui crate itself, split
roughly half frontend (check --tests 14 s) and half codegen + link (28 s
total); dependencies and the linker are not where the time is.

`cargo llvm-lines -p codewhale-tui --lib`: **8,138,810 lines in 223,052
copies**. Largest single function is the `rust_i18n` backend closure
(`_RUST_I18N_BACKEND::{closure#0}`, 311,782 lines, 3.8 % of the crate on
its own — the 15 locale packs are compiled into a match by the `i18n!`
macro), then `run_event_loop` 27 k, `Engine::run_turn` 26 k,
`RuntimeThreadManager::monitor_turn` 16 k, then serde `Deserialize`
expansions for `Config`/`ProvidersConfig`/`Settings` (5–6 k each, several
copies per toml deserializer).

### A0.1 dependency ratchet

`cargo metadata --locked` counted **690** packages and `cargo deny check
bans` warned on duplicate `fancy-regex`, `jsonschema`, `jsonschema-regex`,
`referencing` plus stale `jni`/`jni-sys`/`redox_syscall` skips. Cause: the
workspace `jsonschema` pin had been bumped to 0.49 while `schemaui` 0.12
(latest 0.12.4 included) still requires `^0.46`. Pinning the workspace
back to the 0.46 line removes the second jsonschema stack (**685**
packages; deny bans and advisories clean; `--locked` resolves;
codewhale-workflow-js 61 tests and the tui schema tests pass). Cold saving
is the two duplicated units (~9 s of unit time, ~3 s of wall).

### A1 cache topology (desk-local, not committed)

New-worktree cold `cargo test -p codewhale-tui --lib --locked --no-run`,
same machine, back to back:

| Topology | Wall | CPU (user) | Notes |
| --- | --- | --- | --- |
| Per-worktree fresh target (control) | 127 s | 329 s | A0 |
| One shared `CARGO_TARGET_DIR` (warm from another worktree) | 121 s | 188 s | deps reused; every workspace crate recompiles (path-keyed); no lock waits observed; target 14 GB |
| `build.build-dir = ".../{workspace-path-hash}"` per workspace + warm shared `sccache` (`CARGO_INCREMENTAL=0`) | 107 s | 161 s | 73.6 % sccache hit rate (all 337 Rust dep units hit; the 125 misses are workspace crates); 2.7 GB build dir per workspace + 483 MB cache; the same command that *populated* the cache took 108 s / 157 s CPU |

Wall time is the tui crate in every topology; the topologies buy CPU
(~50 %), which is what matters when several checkouts build at once.
Recommended user-level `~/.cargo/config.toml` (adjust the two roots):

```toml
[build]
# One build root for every checkout; each workspace gets its own subdir,
# so worktrees never wait on each other's target lock.
build-dir = "/path/to/cache/codewhale/build/{workspace-path-hash}"
# Optional: reuse dependency compilation across checkouts.
# rustc-wrapper = "sccache"
```

`sccache` was installed with `brew install sccache` on this machine for the
measurement.

### Public helper (A1/A5) — what `dev-test.sh` actually does now

`scripts/dev-test.sh` previously only mapped an area to `cargo test -p`.
It did **not** activate the measured build-dir + sccache topology, so a
new worktree still paid a cold compile into `./target`.

`scripts/dev-cache.sh` is the portable opt-in helper.
`scripts/dev-cargo.sh` and `scripts/dev-test.sh` source it.

| Class | What changed | What it is not |
| --- | --- | --- |
| **Compile-time** | `scripts/dev-test.sh` / `scripts/dev-cargo.sh` set `CARGO_BUILD_BUILD_DIR=$CODEWHALE_CACHE_ROOT/build/{workspace-path-hash}`, so concurrent worktrees do not share a Cargo lock. A leftover `./target` (Cargo still writes `CACHEDIR.TAG` there when build-dir is split) does **not** turn isolation off; `CODEWHALE_DEV_CACHE=local` keeps `./target` if you want that. Cargo older than 1.91 falls back to a per-workspace `CARGO_TARGET_DIR`. | Not a smaller rustc unit. Workspace crates still rebuild. |
| **Compile-time (sccache)** | `RUSTC_WRAPPER=sccache` and `SCCACHE_DIR=$CODEWHALE_CACHE_ROOT/sccache/<rustc-commit>` only when incremental is already off (`CARGO_INCREMENTAL=0` or `CODEWHALE_SCCACHE=1`) **and** `sccache` is on `PATH`. | Not enabled on the everyday incremental loop. sccache cannot cache incremental units; wrapping those builds adds overhead and 0% hits. Missing sccache is a printed fallback, not an error. |
| **Test-runtime** | `scripts/dev-test.sh` uses `cargo nextest run` when `cargo-nextest` is installed (`CODEWHALE_DEV_NEXTEST=0` forces libtest). Same binaries; process per test. Retries stay 0. `RUST_MIN_STACK=16MiB` is exported when unset. | Not a compile win. nextest does not run doctests; `cargo test --doc` remains a separate gate. |
| **Ergonomics** | `--list` and path mapping cover every workspace crate (`app-server`, `workflow-js`, …). `scripts/dev-cache.sh --status` / `--self-check` print the topology. | No product behavior change. |

Defaults never contain a machine-specific absolute path:

```sh
# Portable default:
#   ${XDG_CACHE_HOME:-$HOME/.cache}/codewhale
# Desk override, if you want the cache on a particular volume:
export CODEWHALE_CACHE_ROOT=/path/to/cache/codewhale

scripts/dev-cache.sh --self-check
scripts/dev-test.sh crates/command-contract/src/elapsed.rs
CARGO_INCREMENTAL=0 scripts/dev-cargo.sh test -p codewhale-config --lib --locked --no-run
```

Hermetic script test (no rustc compile): `sh scripts/dev-cache.test.sh`.
When the repository build lock governs Cargo, the entry point disables the
external `cargowhale` governor for that process. Running both governors can
replace the selected cache and recreate deleted tracked files while forcing
their timestamps. Keep one build owner and a frozen source manifest; a build
helper must not change that source. This leaves the machine's global settings
unchanged.
`scripts/dev-test.sh --self-check` reports the helper's resolved cache
topology; its own script test, `scripts/dev-test.test.sh`, was removed in
`d64b9429b7`.

### Helper verification (2026-08-15, this worktree)

Recorded after other lanes released the machine (load 3.2–5.6). rustc
1.97.0, cargo 1.97.0, sccache 0.17.0. `CODEWHALE_CACHE_ROOT` set to a
volume-local override for the run; no caches or targets were deleted.

Cargo expands `{workspace-path-hash}` to `build/d4/96565f96fb3682` for
this worktree. The first isolated `codewhale-config` `--no-run` created a
stub `./target` (`CACHEDIR.TAG`); treating that as a warm traditional
target made the next command recompile into `./target` (8.65 s). The
helper now stays isolated unless `CODEWHALE_DEV_CACHE=local` or `0`.

**Compile-time** (`scripts/dev-cargo.sh test … --locked --offline --no-run`):

| Step | Wall | Notes |
| --- | ---: | --- |
| First isolated `codewhale-config --lib --no-run` | 9.14 s (user 23.8 s) | 90 units into the hashed build-dir |
| Warm isolated same command (after the stub-target fix) | 0.13 s | `Finished` in 0.07 s |
| `touch crates/config/src/lib.rs` + isolated `--no-run` | 0.93 s | only `codewhale-config` rebuilt |
| First isolated `codewhale-tui --lib --no-run` | **121.5 s** (user 305 s) | 600 units; 340 MB binary; A0 empty-target was 127 s / 329 s |
| `touch crates/runtime/src/elapsed.rs` + isolated `--no-run` | **18.15 s** | everyday compile loop; A0 was 21 s / 19 s |
| `CODEWHALE_SCCACHE=1` config `--no-run` on the already-warm tree | 5.21 s then 0.14 s | wrapper and `SCCACHE_DIR=…/sccache/<rustc-commit>` set; 0 sccache hits because only workspace crates recompiled and the build-dir was not emptied |

**Test-runtime**:

| Step | Wall | Notes |
| --- | ---: | --- |
| `scripts/dev-test.sh config` (nextest, 557 tests) | run 0.479 s / real 2.40 s | includes a 0.85 s profile flip compile |
| `CODEWHALE_DEV_NEXTEST=0 scripts/dev-test.sh config` (libtest) | body 0.11 s / real 0.27 s | 557 tiny tests: process-per-test is slower here |
| `scripts/dev-test.sh crates/runtime/src/elapsed.rs` | run 0.023 s / real 2.81 s | 4 passed, 10,516 skipped; nextest filter works |

The 268 s → ~100 s nextest win remains the earlier tui-unit-suite receipt.
Config is too small for that win; nextest is still the right default for
unfiltered crate/workspace runs.

**Ergonomics:** `sh` and `dash` both passed `dev-cache.test.sh` (22) and
`dev-test.test.sh` (27) at the time; `dev-test.test.sh` has since been removed
(`d64b9429b7`). Missing sccache is a fallback. `--list` covers
every workspace crate.

### A2 nextest in CI

`cargo test --workspace --all-features --locked --doc` inventoried
**3 passing / 8 ignored doctests across 21 crates**. All 8 ignored fences
were in codewhale-tui, which has no runnable doc example and now sets
`doctest = false`, so its doc pass is skipped (the new inventory is not
re-measured yet). CI keeps the doc run as a separate step next to
`cargo nextest run --workspace --all-features --locked --profile ci`.

### A3/A4 (not adopted, measured)

Frontend and codegen split the tui unit roughly evenly (14 s / 14 s
incremental); the linker is a small part of that and dependencies are
already warm after the first build, so `[profile.dev.package."*"]
opt-level = 1` (paired result above), `-Wl,-dead_strip`, and other
`RUSTFLAGS` stay out of the repo (they would apply to shipped profiles);
`split-debuginfo` is already `unpacked` on macOS.

## Peak memory (why OHOS/Windows builds see two ~4 GB rustc processes)

Sampled `ps -o rss` once a second for every rustc under this lane's target
dir (`backups/compile-speed-evidence-20260815/rss-sample.sh`,
`mem-incremental.log`, `mem-cold-cgu.log`); one rustc per row.

| Unit | Mode | Peak RSS | Wall | Load |
| --- | --- | --- | --- | --- |
| `codewhale-tui` lib (dev) | incremental, cgu 256 | 3.3 GB | 12–14 s | 6.3 |
| `codewhale-tui` lib test | incremental, cgu 256 | 6.0 GB | 21–28 s | 6.3 |
| `codewhale-tui` lib (dev) | non-incremental (`CARGO_INCREMENTAL=0`), cgu 16 | **6.0 GB** | 78 s | 5.5 |
| `codewhale-tui` lib test | non-incremental, cgu 16 | **8.0 GB** | 105 s | 5.5 |
| `codewhale-tui` lib test | non-incremental, `codegen-units = 4` | 6.1 GB (−24 %) | 145 s (+38 %) | 5.5 |
| `codewhale-tui` lib test | non-incremental, `codegen-units = 1` | 7.8 GB (−3 %) | 161 s (+53 %) | 5.5 |
| next-largest units (codewhale-config, rmcp, tokio, schemaui, codewhale-workflow) | either | 0.4–0.7 GB | — | — |

So a plain `cargo build -p codewhale-tui` needs ~6 GB for one rustc, the
unit-test build ~8 GB, and `cargo test --workspace` (or `--all-targets`)
schedules the lib and lib-test units of the tui crate concurrently with
the CLI, which is exactly the "two rustc processes at ~4 GB each" a
community member reported while cross-compiling for OHOS on Windows (RSS
accounting differs by OS; the shape is the same). The inline test modules
add ~2 GB (+33 %) to the crate's peak; generic bloat is the driver on both
axes (8.1 M LLVM lines, `rust_i18n` closure 312 k, serde `Deserialize`
expansions for the config structs). Fewer codegen units trade a little
peak for a lot of wall time and are not adopted by default.

### Low-memory build recipe (machines with < 16 GB, cross-builds)

```bash
# One rustc at a time: the tui lib and its unit-test build never overlap.
export CARGO_BUILD_JOBS=1            # or: cargo build -j1 ...
# Only the crate you are working on, only its library:
cargo build -p codewhale-tui
cargo test  -p codewhale-tui --lib -- <filter>
# Do NOT use --workspace/--all-targets on a small machine; run crates one
# at a time (scripts/dev-test.sh <area> picks the narrowest command).
# Optional, if 8 GB for the unit-test build is still too much (slower):
export CARGO_PROFILE_DEV_CODEGEN_UNITS=4   # ~6 GB peak, ~+40 % wall
# Cross-builds (e.g. OHOS) inherit the same numbers: add -j1 to the
# cargo/ohrs invocation and build the release profile, which peaks lower
# than the unit-test build because it carries no test modules.
```

### B1 (megatest peel) — audited, not landable under the constraints

The six largest inline test files (tui/ui/tests.rs 22.1 k lines / 643
tests, tools/subagent/tests.rs 18.7 k / 448, core/engine/tests.rs 17.8 k /
358, config/tests.rs 12.7 k / 393, runtime_threads/tests.rs 9.3 k / 141,
runtime_api/tests.rs 9.1 k / 151) reference crate internals 826 / 226 /
627 / 85 / 152 / 217 times respectively (`crate::llm_client::mock`,
`crate::test_support::{EnvVarGuard, lock_test_env}`,
`core::engine::mock_engine_handle`, `crate::tui::app::App`, …), and the
codewhale-tui library exposes four `pub` items in total. Every one of them
is white-box; none can move to `crates/tui/tests/` without making the
module tree public, which this lane was told not to do. The lever this
would have bought — the ~2 GB / ~35 s that the test modules add to the
lib-test unit — needs a decision first: either a `#[doc(hidden)] pub mod
test_api` (a deliberately public, unstable surface for the ~30 symbols the
black-box subsets use) or accepting that the unit suite stays inside the
crate. Recorded here rather than done.

### B2 landed (leaf types out of codewhale-tui)

| Move | Lines out of tui | Consumers changed |
| --- | --- | --- |
| `core/tool_parser.rs` → `codewhale_core::tool_parser` | 662 | 0 (re-export; integration harness imports instead of `#[path]`) |
| `tls.rs` → `codewhale_release::tls` | 21 | 0 (`use codewhale_release::tls;` at the crate root) |
| `AppMode` (+ pure impl) → `codewhale_config::AppMode`; localized picker strings stay as `AppModeUi` | ~150 | 3 files import the trait |
| `ApprovalMode` (+ pure impl) → `codewhale_execpolicy::ApprovalMode` | ~60 | 0 (re-export) |

Together ~0.9 k of the crate's 746 k lines: correct dependency direction
established, no measurable change to the tui unit's time or RAM yet (the
lib-test peak above, 8.0 GB, was sampled after these moves). Not moved,
with the reason: `ReasoningEffort` — its impl takes the TUI-defined
`ApiProvider` (`crates/tui/src/config.rs`) and calls
`crate::config::is_exact_*_k3_route` / `crate::provider_lake`, so
`ApiProvider` has to move first (B3, below); `approval/policy.rs` (risk
classify) depends on `command_safety` and `auto_review`; `hashing.rs` is
15 lines of sha2 wrappers with 53 call sites and no compile-time value on
its own; the uncompiled `core/runtime_contract/{budget,context,ledger,
manifest,profile,progress,retry,terminal,work}.rs` have zero consumers and
zero build cost (`core/mod.rs` documents them as staged scaffolding,
TUI-DOG-017) — left as they are.

### B3 order

1. `ApiProvider` + the exact-route helpers (`is_exact_*_route`) out of
   `crates/tui/src/config.rs` into codewhale-config, unblocking
   `ReasoningEffort`. **Not started, and now the critical path** — see
   item 4.
2. **Landed.** `localization` + `locales/*.json` →
   `codewhale-localization` (the 312 k-line `rust_i18n` closure left the
   tui unit; locale-only edits no longer rebuild the TUI).
3. **Landed.** `palette` → `codewhale-palette`; `command_safety` →
   `codewhale-execpolicy` (it already owned `ApprovalMode`, so that move
   removes a dependency edge rather than adding one).
4. `client/` (provider wire adapters) → `codewhale-client`: **blocked on
   item 1, not merely ordered after it.** With doc comments and
   `#[cfg(test)]` blocks excluded, `client` still has 20 production
   `crate::` edges. Three of them are hard:
   - `crate::config` — `Config`, `ProvidersConfig`, `ProviderConfig`,
     `TuiConfig`, `ApiProvider`, `RetryPolicy`, `validate_route`,
     `wire_model_for_provider_route` and ~130 provider base-URL / model-id
     constants. `crates/tui/src/config` is 29.7 k lines and itself reaches
     `config_persistence`, `oauth`, `credentials`, `tui`, `fleet`,
     `goal_loop`, `sandbox`, `lsp` … in production, so it cannot follow
     `client` out.
   - `crate::tools` ⇄ `client` is a genuine cycle: `client` uses
     `tools::schema_sanitize`, `tools::large_output_router` and
     `tools::truncate`, while `tools/{spec,review,registry,rlm,verify,
     speech,fim,web_search,web/backend,subagent/advisor}.rs` use
     `client::{CodewhaleClient, ProviderNativeSearchClient,
     ProviderNativeSearchRequest, SpeechSynthesisRequest,
     RemoteControlInferencePermit}`.
   - `crate::core` ⇄ `client` is the same shape: `client` uses
     `core::events::bounded_tool_projection_warning_names`, while
     `core/{engine,engine/preview,engine/dispatch,engine/turn_loop,
     engine/reviewer,protocol_parity}.rs` use `client::{CodewhaleClient,
     PreparedOutboundRequest, canonical_json, parse_usage,
     is_reasoning_replay_placeholder, redact_url_for_display}`.
   Item 1 is therefore the whole precondition: move `ApiProvider`, the
   exact-route helpers and the provider constants into codewhale-config
   first, then re-measure the `tools` and `core` cycles.
5. **Landed as the tractable part of item 4.** `models` +
   `model_catalog` → `codewhale-models` (1,835 lines, 140 consumer files).
   These sit directly under `client` on its dependency spine, had exactly
   one production edge between them and none to the rest of the TUI, and
   `models` was already half a re-export facade over
   `codewhale_core::{request, role}`.
6. Then `fleet/`, `tools/`, `core/engine` — each behind the crate boundary
   its tests already respect, measured with the A0 table.

## One build per machine

`scripts/dev-cargo.sh` and `scripts/dev-test.sh` hold an exclusive machine-wide
build lock (`<cache root>/build.lock`, via `scripts/build-lock.py`) for the
whole Cargo invocation. Cargo's own lock is per target directory, so two
agents building into different target dirs still ran concurrently and exhausted
memory. A second build waits and prints who holds the lock. Set
`CODEWHALE_BUILD_LOCK=0` to skip it, or `CODEWHALE_BUILD_LOCK_FILE` to name the
lock file. A self-hosted CI runner on the same machine joins the lock when its
`.env` sets `CODEWHALE_BUILD_LOCK_FILE` to the same path: the macOS Test job
then holds it from the first test build to the end of the job. The lock is advisory: Cargo started
directly, outside these scripts, does not take it, and on platforms without
`fcntl` (Windows) the build runs unlocked after a warning.

## What changed (this lane)

1. **`scripts/dev-cache.sh` / `scripts/dev-cargo.sh` activate the measured
   isolated build-dir topology** from `scripts/dev-test.sh`. New worktrees
   no longer compile into a private cold `./target` unless the helper is
   disabled. sccache is opt-in and incremental-gated. Script self-checks
   live in `scripts/dev-cache.test.sh` and `scripts/dev-test.sh --self-check`
   (`scripts/dev-test.test.sh` was removed in `d64b9429b7`).
2. **`cargo nextest` is supported and documented** (`.config/nextest.toml`).
   Same test binaries, one process per test, so the tui unit suite runs in
   ~100 s instead of ~270 s here and slow or hanging tests are named instead
   of stalling the binary. The PTY binary is pinned to one test at a time
   (it drives pseudo-terminals and shared mock servers; today it serializes
   on an in-process mutex, which nextest's process-per-test model would
   otherwise bypass), and the integration binary that spawns the real
   `codewhale` executable is capped at four concurrent tests so its 30 s
   start-up budgets survive a fully loaded machine.
   `cargo test --workspace --all-features --locked` remains the
   authoritative gate; nextest is the local loop.
3. **Three tests depended on test order** and only passed because another
   test in the same process had installed the rustls crypto provider first:
   `codewhale-tui mcp::sse::endpoint_tests::message_before_endpoint_is_rejected_instead_of_buffered`,
   `codewhale-app-server tests::failed_config_set_keeps_the_stdio_bridge`,
   and `tests::successful_config_set_still_invalidates_the_stdio_bridge`.
   Each now installs the provider itself, exactly as production does at
   startup. No runtime code changed.
4. **CONTRIBUTING.md has a "Fast local loop" section**: `scripts/dev-cargo.sh`
   / `scripts/dev-test.sh` first, targeted `-p` filters, nextest, isolated
   per-worktree build dirs, and the optional accelerators below. A shared
   `CARGO_TARGET_DIR` is documented only for serialized trunk work.

## Measured and deliberately not adopted

- `[profile.dev.package."*"] opt-level = 1` (dependencies optimized once,
  workspace crates untouched). Paired measurement, back to back, same
  target layout: cold `cargo test -p codewhale-tui --lib --no-run` from an
  empty target went 148 s → 193 s (user 347 s → 778 s); the tui unit suite
  under nextest went 96 s → 81 s; incremental rebuilds are unchanged. A
  ~15 % faster test run is not worth a 2.2× more expensive cold build for
  people trying to build Codewhale for the first time. Contributors who
  mostly re-run tests can opt in locally by adding that table to a
  user-level `~/.cargo/config.toml` `[profile.dev.package."*"]` section.
- Extra `RUSTFLAGS`/linker flags in a repo `.cargo/config.toml`
  (`-no_deduplicate`, alternative linkers). Rustflags apply to every
  profile and would change shipped binaries; the macOS system linker is
  already `ld-prime`, and the measured incremental link cost is inside the
  12–19 s incremental numbers above. Documented as optional local
  accelerators instead.

## Deferred: split `codewhale-tui`

The single lever left that changes the shape of the numbers is splitting
the crate so a change to a leaf module does not re-typecheck 600k lines
and re-link a 357 MB test binary. Mechanical candidates, in dependency
order (each already only depends on `codewhale-config`/`codewhale-paths`
plus third-party crates, and each is consumed through a single module
path today):

| Candidate crate | From | Why it is a clean cut | Consumers to re-export from |
| --- | --- | --- | --- |
| `codewhale-glyphs` | `crates/tui/src/tui/glyphs.rs` | Constant tables + pure fns; no crate-internal deps. | `crate::tui::glyphs` |
| `codewhale-i18n` | `crates/localization/src/lib.rs` + `crates/localization/locales/*.json` | The `rust_i18n::i18n!` macro compiles all 15 packs into whichever crate hosts it; moving it out means locale-only edits no longer rebuild the TUI. `MessageId` is a plain enum. | `crate::localization` |
| `codewhale-mcp-transport` | `crates/tui/src/mcp/{sse,stdio,external_import}.rs` | Already talks to `codewhale-mcp`; the reviewed-launch binding is the only tui coupling. | `crate::mcp` |

Rules for the split: pure moves plus `pub use` re-exports at the old
paths, no behavior change, one crate per PR, each PR measured with the
table above (cold build, incremental build, incremental test build,
`cargo test -p codewhale-tui --lib --no-run`). Expected win: the tui
frontend time drops with lines removed; the test-binary link is unchanged
until the tests that live with those modules move with them.

## Optional accelerators (not required)

- `cargo install cargo-nextest` — see above.
- `scripts/dev-cargo.sh` / `scripts/dev-test.sh` — isolated `build-dir`
  per worktree plus optional sccache. Override the root with
  `CODEWHALE_CACHE_ROOT`; do not commit a machine path.
- One shared `CARGO_TARGET_DIR` only for serialized trunk work (two
  cargos on the same target flock). Prefer the helper above.
- `sccache` as `RUSTC_WRAPPER` caches dependency compilation across clean
  checkouts when `CARGO_INCREMENTAL=0`, and matches what CI does
  (`.github/workflows/ci.yml` uses `mozilla-actions/sccache-action` plus
  `Swatinem/rust-cache`).
