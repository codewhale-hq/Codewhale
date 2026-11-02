# Legacy `.deepseek/` compatibility paths — audit & migration status (#3068)

> 阅读简体中文版：[zh_hans/LEGACY_PATHS.md](zh_hans/LEGACY_PATHS.md)。

## v0.9.10 cleanse ledger

This is the release-window decision record for residue that can otherwise look
interchangeable in a text search. A compatibility entry must name the contract
that still requires it; new production code must use a current path.

| Class | Surface | Decision / contract |
|---|---|---|
| DELETE | Retired underwater build-version test override | Removed in `1cc4d1d1b`; the owner test was already deleted and the production-only build version remains authoritative. |
| MIGRATE | Cross-task Agent Mail | `bd998495a` and managed-app `4ff7bc50d` replace guessed event aliases and same-session mailbox conflation with one durable typed runtime protocol. |
| MIGRATE | TUI `status_message` writes | Active residue; touched flows must move to the typed toast/current-state owner. No new writes are allowed. |
| COMPATIBILITY | `.deepseek/` state and stored provider aliases | Read fallback for upgraded installations and stored provider identity; writes remain Codewhale-only. Details are below. |
| COMPATIBILITY | `agent_message` transcript/protocol decoding | Persisted transcripts and tool wire values require decoding; it is not the cross-task Agent Mail transport. |
| COMPATIBILITY | Managed runtime envelope schema 1 | Previously persisted/enrolled runner replay; current local Codewhale Agent Mail producer is schema 2 and both normalize to the managed envelope. |
| CURRENT | DeepSeek provider support | A real selectable provider, not product branding; provider-scoped names and configuration remain. |
| CURRENT | Same-session subagent mailbox | Start/status/peek/message/followup/interrupt/wait/cancel stays scoped to a parent runtime session. |
| CURRENT | Web locale dictionaries | The application and documentation routes have no page-local `isZh` branches; new copy continues through dictionaries. |
| CURRENT | Safety, authorization, protocol, persistence, migration, and data-integrity tests | These protect external or durable contracts and are not copy/layout cleanup candidates. |

Snapshot on 2026-08-19: `10,954` Rust `#[test]` attributes under
`crates/tui`, `523` TUI `status_message` source lines, and zero page-local
`isZh` files under `apps/web/app` in the paired managed-app candidate. Counts
are inventory signals, not deletion targets.

Codewhale was renamed from DeepSeek-TUI. To avoid breaking existing installs, the runtime reads
state from the new `~/.codewhale/` location but **falls back** to the legacy `~/.deepseek/` location,
and always **writes** to `~/.codewhale/`. This doc audits each legacy reference and records a
keep / deprecate / remove decision so the migration is auditable.

## The canonical resolver (use this for new code)

State-dir APIs are exported by `codewhale-config`; their implementation is
`crates/config/src/state_dirs.rs`, with root selection in `crates/paths/src/lib.rs`.
The paths in this audit show defaults. `CODEWHALE_HOME` selects the primary
home and prevents fallback outside that root.

| Symbol | Purpose |
|---|---|
| `CODEWHALE_APP_DIR` / `LEGACY_APP_DIR` | Default directory names from `codewhale-paths`. |
| `codewhale_home()` | Selected primary home, defaulting to `~/.codewhale`. |
| `legacy_deepseek_home()` | Selected legacy home, defaulting to `~/.deepseek`. |
| `resolve_state_dir(subdir)` | Read the primary directory; fall back only if it is absent and no explicit primary home was selected. |
| `ensure_state_dir(subdir)` | Write under the primary home; guarded migration can relocate owned legacy state and reports what moved. |

Compatible harness roots are read-only discovery inputs, not owned migration
or deletion targets.

## Per-path decisions

**Decision for all legacy references below: keep-as-fallback.** Removing the `.deepseek` fallback
would strand users who upgraded in place and never re-ran onboarding. Revisit only after a release
that actively migrates `~/.deepseek/` → `~/.codewhale/` on first run and a deprecation window.

| Reference | Routed through `resolve_state_dir`? | Decision |
|---|---|---|
| `config::resolve_state_dir` / `ensure_state_dir` | n/a (the resolver itself) | keep — canonical |
| `crates/skills/src/roots.rs` (`~/.deepseek/skills`) | no — compatible-root enumeration | Keep read-only harness discovery; this is not an owned write target. |
| `crates/engine/src/prompts.rs` (`LEGACY_HANDOFF_RELATIVE_PATH = ".deepseek/handoff.md"`) | no — explicit legacy const | keep — explicit legacy handoff fallback |
| `crates/tui/src/workspace_trust.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/engine/src/session_manager.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/runtime/src/skill_state.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/engine/src/tools/skill.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/snapshot/src/lib.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/runtime/src/workspace_discovery.rs` | no — hardcoded | keep-as-fallback; follow-up |

## Follow-up source changes

The table retains the original audit with moved owners corrected. Verify each
current caller before changing it. Reuse its existing issue and resolver;
preserve explicit-home isolation, read fallback, ownership and recovery.
Use existing focused checks. This document does not authorize moving or
deleting another harness's roots.
