# RFC: File Decomposition for v0.9.0

**Status (2026-07-12): still valuable, needs redesign.** The original
`config.rs` plan was overtaken by events: `config.rs` shrank to ~7.0k lines via
a different module split (`crates/tui/src/config/`), and the provider-churn
problem it targeted was mostly solved by the Models.dev catalog + ProviderLake
(#4184–#4188) rather than by file moves. Meanwhile the render/CLI monoliths
grew — `ui.rs` 9.4k → 13.6k, `main.rs` 8.0k → 12.1k — so any revival of this
RFC should re-scope around `ui.rs` and `main.rs`. The migration method
(§"Migration strategy": git mv + re-exports, no functional changes) remains
the right approach.

## Problem

Six files exceed 5,000 lines. The worst offenders accumulate provider-specific
logic, test code, and UI rendering in single translation units. This makes
provider additions touch 15+ files and makes code review fragile.

### Current state (lines, v0.8.6x era — see status note for v0.8.68 numbers)

| File | Lines | Contents |
|------|-------|----------|
| `crates/tui/src/config.rs` | 10,046 | Provider resolution, env handling, model aliases, capability matrix, 2,000+ lines of tests |
| `crates/tui/src/tui/ui.rs` | 9,400 | TUI render loop, input handling, command dispatch, /logout clearing |
| `crates/tui/src/tui/ui/tests.rs` | 8,360 | Tests for ui.rs |
| `crates/tui/src/main.rs` | 7,998 | CLI arg parsing, mode selection, startup |
| `crates/tui/src/tui/app.rs` | 7,256 | Application state struct and lifecycle |
| `crates/engine/src/runtime_threads.rs` | 5,454 | Async runtime orchestration |

## Proposed decomposition

### 1. `config.rs` → provider module tree

Split `crates/tui/src/config.rs` into:

```
crates/tui/src/config/
├── mod.rs              # Re-exports, Config struct, load/save
├── provider.rs         # ApiProvider enum, parse/as_str/display_name/all
├── capability.rs       # ProviderCapability, provider_capability()
├── model_resolution.rs # wire_model_for_provider, normalize_model_name_for_provider
├── env.rs              # EnvGuard, env var precedence, per-provider env handling
├── constants.rs        # All DEFAULT_*_MODEL and DEFAULT_*_BASE_URL constants
└── tests/              # Test module
    ├── mod.rs
    ├── provider.rs
    ├── capability.rs
    ├── model_resolution.rs
    └── env.rs
```

**Why:** Every new provider currently requires edits to ~20 match arms scattered
across one 10K-line file. With constants in their own module and resolution
logic isolated, adding a provider becomes: add constants, add enum variant, add
one match arm per function. The drift check script can validate each sub-module
independently.

### 2. `ui.rs` → view modules

Split `crates/tui/src/tui/ui.rs` into:

```
crates/tui/src/tui/
├── ui.rs               # Core render loop, frame dispatch (keep under 2,000 lines)
├── input.rs            # Keyboard/mouse input handling
├── command_dispatch.rs # /command routing, /logout, /config
└── status_bar.rs       # Status bar rendering
```

**Why:** The /logout clearing logic, command dispatch, and render loop are
independent concerns. `ui.rs` currently has a 6,200-line function body for
`execute_command_input` that mixes input parsing, command routing, and state
mutation.

### 3. `main.rs` → CLI module

Split `crates/tui/src/main.rs` into:

```
crates/tui/src/cli/
├── mod.rs              # Cli struct, arg parsing
├── args.rs             # Argument definitions
└── startup.rs          # Mode selection, config loading, resume logic
```

**Why:** `main.rs` at 8K lines suggests the CLI definition has outgrown a
single file. Separating arg definitions from startup logic makes the entry
point readable.

### 4. Provider additions should be data-driven

The current provider pattern requires touching:
- `config.rs`: 20+ match arms
- `cli/src/lib.rs`: 4+ match arms
- `agent/src/lib.rs`: static registry
- `tui/provider_picker.rs`: picker list
- `docs/PROVIDERS.md`: registry table
- `config.example.toml`: example section
- `README.md`: env vars table
- `scripts/check-provider-registry.py`: drift check

A data-driven approach would define each provider as a struct with its
constants, env vars, capability metadata, and display name — then derive the
match arms from the data. This is a larger refactor but would reduce provider
additions to a single file change.

## Priority

1. **config.rs decomposition** — highest impact, most provider churn happens here
2. **ui.rs decomposition** — second highest, /logout and command dispatch are independent
3. **Data-driven providers** — aspirational for v0.9.0, requires trait design

## Migration strategy

Each decomposition should be a standalone PR that:
1. Creates the new module tree
2. Moves code with `git mv` (preserves history)
3. Adds `pub use` re-exports in the old file location (zero API change)
4. Runs the full test suite
5. Removes the re-exports in a follow-up PR once consumers are updated

No functional changes in decomposition PRs. Keep them boring.
